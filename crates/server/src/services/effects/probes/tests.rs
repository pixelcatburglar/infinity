//! Real launcher admission for the three published interdiction charges:
//! one Activate consumes one charge and births a stationary probe, a repeat
//! request is ignored, and a targeted or prohibited request spends nothing.
use crate::services::effects::tests::fixture;
use crate::services::effects::lifecycle;
use crate::services::identity::Identity;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipFixture, ShipItemSpec, ShipLocation};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_HI_SLOT_FIRST,
    dogma::{ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_HI_SLOTS, ATTR_POWER, ATTR_POWER_OUTPUT, ATTR_QUANTITY, ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS}};
use std::sync::Arc;

const LAUNCHER: i32 = 22782;
const CHARGES: [i32; 3] = [22778, 34260, 56748];
fn restriction_values(sde: &crate::sde::Sde, module: &crate::sde::types::TypeData, name: impl Fn(i32) -> String) -> Vec<i32> {
    (1..).map_while(|index| sde.dogma.attributes_by_name(&name(index)).next().map(|attribute| attribute.attribute_id))
        .filter_map(|attribute| module.attribute(attribute)).map(|value| value as i32).filter(|value| *value > 0).collect()
}

struct Scene {
    db: crate::db::Db,
    sde: Arc<crate::sde::Sde>,
    world: Arc<crate::space::World>,
    system: i64,
    fixture: ShipFixture,
    tx: crate::space::NotifyTx,
    rx: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<marshal::Value>)>,
}

fn allowed_hull(sde: &crate::sde::Sde, module: &crate::sde::types::TypeData) -> i32 {
    let groups = restriction_values(sde, module, |index| format!("canFitShipGroup{index:02}"));
    let types = restriction_values(sde, module, |index| format!("canFitShipType{index}"));
    sde.types.iter().find(|ship| ship.published == Some(true) && ship.category_id == Some(CATEGORY_SHIP)
        && ship.attribute(ATTR_HI_SLOTS).is_some_and(|slots| slots >= 1.0)
        && (groups.is_empty() && types.is_empty() || groups.contains(&ship.group_id.unwrap_or(0)) || types.contains(&ship.type_id))
        && module.attribute(ATTR_CPU).zip(ship.fit_attribute(ATTR_CPU_OUTPUT)).is_some_and(|(need, out)| out >= need)
        && module.attribute(ATTR_POWER).zip(ship.fit_attribute(ATTR_POWER_OUTPUT)).is_some_and(|(need, out)| out >= need))
        .map(|ship| ship.type_id).expect("published hull admitted by the launcher restrictions")
}

async fn content(db: &crate::db::Db, sde: &Arc<crate::sde::Sde>) -> Arc<crate::space::World> {
    let mut config = crate::config::Config::default();
    config.lua.enabled = true;
    config.lua.scripts_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lua").to_string_lossy().into_owned();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    crate::lua::initialize_content(&config.lua, db, sde).await.expect("content catalogue");
    crate::lua::start(&config, db, sde, &world, &hub).await.expect("content host");
    world
}

async fn prepare(db: &crate::db::Db, sde: &Arc<crate::sde::Sde>, world: &Arc<crate::space::World>, charge: i32, highsec: bool) -> Scene {
    crate::tests::init_config();
    let threshold = crate::config::game().crimewatch.highsec_threshold;
    let system = sde.map.systems().filter(|row| row.security.is_some_and(|security| if highsec { security >= threshold } else { security < threshold }))
        .min_by(|left, right| left.security.unwrap().total_cmp(&right.security.unwrap()))
        .expect("security-bounded system").solar_system_id;
    let module = sde.types.get(LAUNCHER).expect("interdiction sphere launcher");
    let hull = allowed_hull(sde, module);
    let fixture = create_ship(db, hull, ShipLocation::Space { system }, &[ShipItemSpec {
        type_id: LAUNCHER, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[],
    }]).await.unwrap();
    let required: Vec<(i32, i32)> = ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS)
        .filter_map(|(&skill, level)| module.attribute(skill).zip(module.attribute(level)))
        .filter(|(skill, level)| *skill > 0.0 && *level > 0.0)
        .map(|(skill, level)| (skill as i32, level as i32))
        .collect();
    for (skill, level) in required {
        db.execute("INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, 0)",
            &[&fixture.character, &skill, &level]).await.unwrap();
    }
    crate::dao::items::combat::apply_loads(db, fixture.ship, &[(fixture.items[0], FLAG_HI_SLOT_FIRST, charge, 2, 0)]).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(fixture.character, tx.clone());
    fixture::admit(db, world, system, fixture::ball(fixture.ship, hull, fixture.character, Some(fixture.character), 0.0)).await;
    Scene { db: db.clone(), sde: sde.clone(), world: world.clone(), system, fixture, tx, rx }

}

fn identity(scene: &Scene) -> Identity {
    Identity { char_id: scene.fixture.character, ship_id: Some(scene.fixture.ship), corp_id: 1, station_id: None, system_id: Some(scene.system) }
}

async fn activate(scene: &Scene, effect: &str, target: Option<i64>, repeat: i64) -> Result<(), crate::services::CallError> {
    lifecycle::activate(&identity(scene), &scene.db, &scene.world, &scene.sde, Some(scene.system), scene.tx.clone(),
        scene.fixture.account, scene.fixture.items[0], effect, target, repeat, |_| Ok(())).await.map(|_| ())
}

async fn loaded(scene: &Scene) -> i64 {
    let (ship, module) = (scene.fixture.ship, scene.fixture.items[0]);
    scene.world.with(scene.system, move |s| s.loaded_effective(ship, module).map(|(_, _, quantity, _)| quantity)).await.unwrap().unwrap()
}

fn quantity_notice(scene: &mut Scene, charge: i32) -> bool {
    let ship = scene.fixture.ship;
    let key = crate::services::combat::targeting_charges::charge_key(ship, FLAG_HI_SLOT_FIRST, charge);
    std::iter::from_fn(|| scene.rx.try_recv().ok()).any(|(name, args)| name == "OnModuleAttributeChanges" && args.first()
        .and_then(|value| value.items()).is_some_and(|rows| rows.iter().any(|row| row.items().is_some_and(|fields|
            fields.get(2) == Some(&key) && fields.get(3).and_then(marshal::Value::as_int) == Some(i64::from(ATTR_QUANTITY))
            && fields.get(5).and_then(marshal::Value::as_int) == Some(1) && fields.get(6).and_then(marshal::Value::as_int) == Some(2)))))
}

async fn finish(scene: Scene) {
    scene.world.remove_ball(scene.system, scene.fixture.ship).await;
    cleanup_ship(&scene.db, Some(&scene.world), scene.fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn launcher_admission_covers_consumption_repeat_and_refusal() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let world = content(&db, &sde).await;
    for charge in CHARGES {
        let mut scene = prepare(&db, &sde, &world, charge, false).await;
        activate(&scene, "useMissiles", None, 0).await.unwrap();
        for _ in 0..80 {
            let born = scene.world.with(scene.system, move |s| s.balls().any(|ball| ball.type_id == charge && ball.mode == crate::space::MODE_STOP)).await.unwrap_or(false);
            if born { break; }
            assert!(scene.world.after(scene.system, 100).await);
        }
        let born = scene.world.with(scene.system, move |s| s.balls().find(|ball| ball.type_id == charge).map(|ball| {
            let time = ball.slim.iter().find(|(key, _)| *key == "warpDisruptionStartTime").map(|(_, value)| value.clone());
            (ball.id, ball.mode, time)
        })).await.unwrap().expect("stationary probe");
        assert_eq!(born.1, crate::space::MODE_STOP);
        assert_eq!(born.2.is_some_and(|value| !matches!(value, marshal::Value::None)), charge != 56748);
        assert_eq!(loaded(&scene).await, 1);
        assert!(quantity_notice(&mut scene, charge));
        scene.world.remove_ball(scene.system, born.0).await;
        finish(scene).await;
    }
    let repeated = prepare(&db, &sde, &world, CHARGES[0], false).await;
    activate(&repeated, "useMissiles", None, 5).await.unwrap();
    activate(&repeated, "useMissiles", None, 5).await.unwrap();
    assert!(repeated.world.after(repeated.system, 5_000).await);
    assert_eq!(repeated.world.with(repeated.system, |s| s.balls().filter(|ball| ball.type_id == CHARGES[0]).count()).await.unwrap(), 1);
    assert_eq!(loaded(&repeated).await, 1);
    finish(repeated).await;
    let refused = prepare(&db, &sde, &world, CHARGES[0], false).await;
    let refused_owner = refused.fixture.character;
    assert!(activate(&refused, "useMissiles", Some(refused.fixture.ship), 0).await.is_err());
    assert!(activate(&refused, "probeLaunching", None, 0).await.is_err());
    let prohibited = prepare(&db, &sde, &world, CHARGES[0], true).await;
    assert!(activate(&prohibited, "useMissiles", None, 0).await.is_err());
    assert_eq!(loaded(&refused).await, 2);
    assert_eq!(loaded(&prohibited).await, 2);
    assert_eq!(refused.world.with(refused.system, move |s| s.balls().filter(|ball| ball.type_id == CHARGES[0] && ball.owner_id == refused_owner).count()).await.unwrap(), 0);
    finish(refused).await;
    finish(prohibited).await;
}
