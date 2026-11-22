//! Paste repair against the real database and system actor: cargo
//! consumption, timed and early settlement, and departure settlement.
use super::*;
use crate::client::filetime_now;
use crate::dao::items::transfers;
use crate::services::tests::fixture::ShipFixture;
use crate::space::combat::tests::cached_module_attribute;
use infinity_core::consts::{dogma::{ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_OUTPUT, ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP, EFFECT_ONLINE, EFFECT_MICROWARPDRIVE}, CATEGORY_MODULE, FILETIME_MINUTE, FLAG_CARGO, TYPE_NANITE_REPAIR_PASTE};
use infinity_core::messages;

/// SDE Nanite Interfacing skill type (`inventorycommon/const.py` typeNaniteInterfacing).
const NANITE_INTERFACING: i32 = 28880;
/// Repair rate with Nanite Interfacing 5: 10 HP/min base * (1 + 0.2 * 5).
const REPAIR_RATE: f64 = 20.0;
/// SDE dogma attribute moduleRepairRate.
const ATTR_MODULE_REPAIR_RATE: i32 = 1267;
/// SDE dogma attribute shipBrokenModuleRepairCostMultiplier.
const ATTR_MODULE_REPAIR_COST_MULTIPLIER: i32 = 1277;
/// Small fixture damage so the timer case completes in under a second.
const EXPIRING_DAMAGE: f64 = 0.3;

/// Settlement bounds from FILETIME brackets: `start`/`stop` each bracket the
/// moment `finish` sampled `filetime_now`, so damage sits between
/// `initial - rate * max_elapsed` and `initial - rate * min_elapsed` minutes.
fn assert_settled(damage: f64, initial: f64, min_elapsed: i64, max_elapsed: i64) {
    let (min, max) = (min_elapsed as f64 / FILETIME_MINUTE as f64, max_elapsed as f64 / FILETIME_MINUTE as f64);
    assert!(damage >= initial - REPAIR_RATE * max - 1e-9 && damage <= initial - REPAIR_RATE * min + 1e-9,
        "settled damage {damage} outside {initial} - {REPAIR_RATE} * [{min}, {max}] minutes");
}

async fn damage_of(db: &crate::db::Db, module: i64) -> f64 {
    dogma::overrides_for(db, &[module]).await.unwrap().iter()
        .find(|v| v.item_id == module && v.attribute_id == ATTR_HULL_DAMAGE).map(|v| v.value).unwrap_or(0.0)
}

async fn quantity_of(db: &crate::db::Db, item: i64) -> i64 {
    db.query_opt("SELECT quantity FROM game.items WHERE item_id = $1", &[&item]).await.unwrap()
        .map(|row| row.get::<_, i64>("quantity")).unwrap_or(0)
}

fn drained(rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) -> Vec<String> {
    let mut names = Vec::new();
    while let Ok((name, _)) = rx.try_recv() { names.push(name); }
    names
}

/// A published ship already in system space with the requested damaged
/// modules; the client is bound to the same world the ship was admitted to.
async fn rig(db: &crate::db::Db, sde: &Arc<crate::sde::Sde>, module_type: i32, damages: &[f64]) -> (Client, tokio::net::TcpStream, tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, Arc<World>, ShipFixture, Vec<i32>) {
    let data = sde.types.get(module_type).expect("fixture module");
    let (_, slots, first) = crate::services::effects::fitting::rack(&sde.types, module_type).expect("module rack");
    let count = damages.len() as f64;
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(slots).unwrap_or(0.0) >= count
        && [(ATTR_CPU, ATTR_CPU_OUTPUT), (ATTR_POWER, ATTR_POWER_OUTPUT)].iter().all(|&(need, output)|
            t.attribute(output).unwrap_or(0.0) >= data.attribute(need).unwrap_or(0.0) * count))
        .expect("published hull with enough slots and fitting resources").type_id;
    let attrs: Vec<Vec<(i32, f64)>> = damages.iter().map(|damage| vec![(ATTR_HULL_DAMAGE, *damage)]).collect();
    let specs: Vec<ShipItemSpec> = damages.iter().enumerate().zip(&attrs).map(|((slot, _), attributes)| ShipItemSpec {
        type_id: module_type, flag_id: first + slot as i32, quantity: 1, singleton: true, online: true, attributes,
    }).collect();
    let f = create_ship(db, hull, ShipLocation::Space { system: SYSTEM }, &specs).await.unwrap();
    let (mut c, peer) = crate::services::tests::client(db.clone(), f.character).await;
    c.session.apply(vec![("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    c.notify_tx = tx.clone();
    let world = c.world.clone();
    world.register_pilot(f.character, tx);
    let required = [module_type, hull].into_iter().flat_map(|ty| crate::services::skills::requirements::requirements(sde, ty))
        .chain([(NANITE_INTERFACING, 5)]).filter(|&(_, level)| level > 0)
        .fold(std::collections::BTreeMap::new(), |mut levels, (skill, level)| {
            levels.entry(skill).and_modify(|old: &mut i32| *old = (*old).max(level)).or_insert(level);
            levels
        });
    let granted_skills = required.keys().copied().collect();
    services::skills::gm::apply(&mut c, f.character, services::skills::gm::Change::Set(required.into_iter().collect())).await.unwrap();
    fixture::admit(db, &world, SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
    (c, peer, rx, world, f, granted_skills)
}

fn module_type(sde: &crate::sde::Sde, activatable: bool) -> i32 {
    sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|hp| hp >= 10.0)
        && t.base_price.is_some_and(|price| price > 0.0)
        && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)
        && (!activatable || t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)))
        .expect("online damageable module").type_id
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn paste_repair_consumes_cargo_times_out_and_settles_early() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let module = module_type(&sde, true);
    let hp = sde.types.get(module).and_then(|t| t.attribute(ATTR_STRUCTURE_HP)).unwrap();
    let effect_name = sde.dogma.effect(EFFECT_MICROWARPDRIVE).and_then(|e| e.name.clone()).expect("MWD effect name");
    let (mut c, _peer, mut rx, world, f, granted_skills) = rig(&db, &sde, module, &[hp / 2.0, EXPIRING_DAMAGE]).await;
    let (a, b) = (f.items[0], f.items[1]);
    let (d, w) = (db.clone(), world.clone());
    let result = tokio::spawn(async move {
        let rate = w.with(SYSTEM, move |s| s.with_combat_fit(f.ship, |fit, _, _| fit.value(Key::Char, ATTR_MODULE_REPAIR_RATE))).await.flatten();
        assert_eq!(rate, Some(REPAIR_RATE), "Nanite Interfacing 5 doubles the resident repair rate");
        let multiplier = w.with(SYSTEM, move |s| s.with_combat_fit(f.ship, |fit, _, _| fit.value(Key::Char, ATTR_MODULE_REPAIR_COST_MULTIPLIER))).await.flatten().unwrap();
        let base = crate::services::repair::computed_base_price(&sde, module);
        let paste_base = sde.types.get(TYPE_NANITE_REPAIR_PASTE).and_then(|t| t.base_price).unwrap();
        let units_of = |damage: f64| (damage * (base / hp) * multiplier / paste_base).ceil() as i64;
        let stack = transfers::insert(&d, TYPE_NANITE_REPAIR_PASTE, f.character, f.ship, FLAG_CARGO, units_of(hp / 2.0) + units_of(EXPIRING_DAMAGE)).await.unwrap();
        drained(&mut rx);
        let start = filetime_now();
        assert_eq!(call(&mut c, "dogmaIM", "InitiateModuleRepair", &[Value::Int(a)], None).await.unwrap(), Value::Bool(true));
        let started = filetime_now();
        assert_eq!(quantity_of(&d, stack).await, units_of(EXPIRING_DAMAGE));
        assert!(drained(&mut rx).iter().any(|name| name == "OnItemChange"));
        assert!(w.after(SYSTEM, 2_000).await);
        let stopping = filetime_now();
        assert_eq!(call(&mut c, "dogmaIM", "StopModuleRepair", &[Value::Int(a)], None).await.unwrap(), Value::Bool(true));
        let stopped = filetime_now();
        let damage = damage_of(&d, a).await;
        assert!(damage > 0.0, "early stop settles a share, got {damage}");
        assert_settled(damage, hp / 2.0, stopping - started, stopped - start);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let cached = w.with(SYSTEM, move |s| cached_module_attribute(s, f.ship, a, ATTR_HULL_DAMAGE)).await.flatten();
                if cached.map(|(value, _)| value) == Some(damage) { break; }
                assert!(w.after(SYSTEM, 100).await);
            }
        }).await.expect("actor fit carries the settled damage");
        assert_eq!(call(&mut c, "dogmaIM", "InitiateModuleRepair", &[Value::Int(b)], None).await.unwrap(), Value::Bool(true));
        let refusal = call(&mut c, "dogmaIM", "Activate", &[Value::Int(b), Value::str(&effect_name), Value::None], None).await;
        assert!(matches!(&refusal, Err(services::CallError::User { msg, dict })
            if msg == messages::MSG_CUSTOM_NOTIFY && *dict == vec![("notify".into(), Value::unicode(messages::heat::BEING_REPAIRED))]),
            "unexpected refusal: {refusal:?}");
        drained(&mut rx);
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if damage_of(&d, b).await == 0.0 { break; }
                assert!(w.after(SYSTEM, 100).await);
            }
        }).await;
        assert!(settled.is_ok(), "paste repair timer settles: damage={}, repairing={}", damage_of(&d, b).await, heat::paste::repairing(b));
        assert!(!heat::paste::repairing(b));
        assert!(drained(&mut rx).iter().any(|name| name == "OnModuleAttributeChanges"));
        dogma::set_override(&d, b, ATTR_HULL_DAMAGE, EXPIRING_DAMAGE).await.unwrap();
        let error = call(&mut c, "dogmaIM", "InitiateModuleRepair", &[Value::Int(b)], None).await.unwrap_err();
        assert!(matches!(&error, services::CallError::User { msg, dict }
            if msg == messages::MSG_NOT_ENOUGH_REPAIR_MATERIAL_TO_FINISH_ALL_REPAIRS && dict.is_empty()), "unexpected: {error:?}");
        dogma::set_override(&d, a, ATTR_HULL_DAMAGE, hp / 2.0).await.unwrap();
        let stack = transfers::insert(&d, TYPE_NANITE_REPAIR_PASTE, f.character, f.ship, FLAG_CARGO, units_of(hp / 2.0)).await.unwrap();
        let result = call(&mut c, "dogmaIM", "InitiateModuleRepairMany", &[Value::list([Value::Int(a), Value::Int(b)])], None).await.unwrap();
        assert_eq!(result, Value::tuple([Value::list([Value::Int(a)]), Value::list([Value::Int(b)]), Value::list([])]));
        assert_eq!(quantity_of(&d, stack).await, 0);
    }).await;
    heat::paste::forget(a);
    heat::paste::forget(b);
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, a, b]).await.unwrap();
    crate::dao::skills::training::remove(&db, f.character, &granted_skills).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn departure_settles_in_progress_repairs() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let module = module_type(&sde, false);
    let hp = sde.types.get(module).and_then(|t| t.attribute(ATTR_STRUCTURE_HP)).unwrap();
    let (mut c, _peer, _rx, world, f, granted_skills) = rig(&db, &sde, module, &[hp / 2.0]).await;
    let a = f.items[0];
    let stack = transfers::insert(&db, TYPE_NANITE_REPAIR_PASTE, f.character, f.ship, FLAG_CARGO, 1_000_000).await.unwrap();
    let start = filetime_now();
    assert_eq!(call(&mut c, "dogmaIM", "InitiateModuleRepair", &[Value::Int(a)], None).await.unwrap(), Value::Bool(true));
    let started = filetime_now();
    let settling = filetime_now();
    combat::reset_pilot(&world, f.character, Some((SYSTEM, f.ship)));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if damage_of(&db, a).await < hp / 2.0 { break; }
            assert!(world.after(SYSTEM, 100).await);
        }
    }).await.expect("departure settles the repair");
    let settled = filetime_now();
    assert!(!heat::paste::repairing(a));
    let damage = damage_of(&db, a).await;
    assert_settled(damage, hp / 2.0, settling - started, settled - start);
    assert_eq!(call(&mut c, "dogmaIM", "StopModuleRepair", &[Value::Int(a)], None).await.unwrap(), Value::Bool(false));
    heat::paste::forget(a);
    crate::dao::skills::training::remove(&db, f.character, &granted_skills).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2", &[&stack, &f.character]).await.unwrap();
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, a, stack]).await.unwrap();
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
}
