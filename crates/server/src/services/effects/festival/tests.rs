//! Real-SDE admission for the two published ordinary launchers and their
//! published compatible charges. Production uses the SDE predicates, so this
//! test fails when the deployed catalogue no longer matches that saved set.
//!
//! The live scene below is the one Activate fixture. The RPC reply only posts
//! the cycle. Payment, flight, contact and expiry are read from the actor.
use super::*;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_STRUCTURE, CATEGORY_STRUCTURE_MODULE};

/// Saved published ordinary launchers: Festival Launcher and Display Launcher.
const LAUNCHERS: [i32; 2] = [19660, 44109];
/// Saved published ordinary group-500 charges whose default effect is snowBallLaunching.
const CHARGES: [i32; 44] = [
    32993, 32994, 32995, 33571, 33572, 33573, 44110, 44260, 44261, 44262, 44263, 44264,
    44265, 44266, 44267, 44268, 44269, 44270, 44271, 44272, 47264, 47302, 49991, 49992,
    50138, 52219, 53343, 53461, 54564, 56745, 56944, 57149, 57150, 60905, 60939, 63165,
    71755, 73408, 74386, 75792, 75821, 76963, 95304, 95305,
];
/// Unpublished group-500 charges, melted group-976 charges and structure types.
const EXCLUDED: [i32; 8] = [24608, 74385, 95357, 95358, 30221, 33569, 47300, 47301];
/// Structure launcher excluded from the ordinary consumer.
const STRUCTURE_LAUNCHER: i32 = 47303;
/// Negative thermal snowball kept in the saved charge set.
const NEGATIVE_CHARGE: i32 = 53343;

fn published_set(sde: &Sde, predicate: impl Fn(&crate::sde::types::TypeData) -> bool) -> Vec<i32> {
    let mut ids: Vec<i32> = sde.types.iter().filter(|row| predicate(row)).map(|row| row.type_id).collect();
    ids.sort_unstable();
    ids
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn published_launchers_and_charges_match_the_saved_set() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let launchers = published_set(&sde, |row| launcher_in_scope(&sde, row.type_id));
    assert_eq!(launchers, LAUNCHERS, "published ordinary festival launchers changed");
    let charges = published_set(&sde, |row| compatible_firework(&sde, row.type_id)
        && LAUNCHERS.iter().all(|launcher| compatible_charge(&sde, *launcher, row.type_id)));
    assert_eq!(charges, CHARGES, "published compatible festival charges changed");
    assert!(CHARGES.contains(&NEGATIVE_CHARGE));
    assert!(sde.types.attribute(NEGATIVE_CHARGE, infinity_core::consts::dogma::ATTR_THERMAL_DAMAGE).is_some_and(|value| value < 0.0));
    for charge in CHARGES {
        assert!(crate::space::damageable::DamageableProjectile::from_sde(&sde, charge).is_some(), "charge {charge} has no projectile profile");
        assert!(sde.types.attribute(charge, ATTR_MAX_VELOCITY).is_some_and(|value| value.is_finite() && value > 0.0), "charge {charge} velocity");
        assert!(sde.types.attribute(charge, ATTR_EXPLOSION_DELAY).is_some_and(|value| value.is_finite() && value > 0.0), "charge {charge} lifetime");
    }
    for id in EXCLUDED {
        assert!(!compatible_firework(&sde, id), "excluded type {id} was admitted");
        assert!(LAUNCHERS.iter().all(|launcher| !launcher_in_scope(&sde, id) || !compatible_charge(&sde, *launcher, id) || !compatible_firework(&sde, id)));
    }
    assert!(!launcher_in_scope(&sde, STRUCTURE_LAUNCHER));
    let structure = sde.types.get(STRUCTURE_LAUNCHER).expect("structure launcher");
    assert!(matches!(structure.category_id, Some(CATEGORY_STRUCTURE | CATEGORY_STRUCTURE_MODULE)) || structure.published != Some(true) || structure.group_id != Some(GROUP_MISSILE_LAUNCHER_FESTIVAL as i32));
    assert_eq!(super::super::registry::kind(&sde, EFFECT_SNOWBALL_LAUNCHING), None);
    assert!(sde.types.iter().any(|row| row.category_id == Some(CATEGORY_CHARGE) && row.group_id == Some(GROUP_FESTIVAL_MISSILE) && !compatible_firework(&sde, row.type_id)));
}

// Live scene helpers. They share one fixture and do not start a second scene.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::services::combat::state;
use crate::services::effects::lifecycle::Module;
use crate::services::effects::tests::fixture::{self, dogma_call};
use crate::services::effects::weapons::consume_charge_in_actor;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use crate::space::combat::inputs::ChargeToken;
use infinity_core::consts::{dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CPU_OUTPUT, ATTR_DRONE_BANDWIDTH, ATTR_DRONE_BANDWIDTH_USED, ATTR_DRONE_CAPACITY, ATTR_HULL_DAMAGE, ATTR_MAX_LOCKED_TARGETS, ATTR_MAX_TARGET_RANGE, ATTR_POWER_OUTPUT, ATTR_RECHARGE_RATE, ATTR_SCAN_RESOLUTION, ATTR_STRUCTURE_HP}, CATEGORY_DRONE, CATEGORY_SHIP, FLAG_CARGO, FLAG_DRONE_BAY, FLAG_HI_SLOT_FIRST};

const FESTIVAL: i32 = 19660;
const DISPLAY: i32 = 44109;
const FIREWORK: i32 = 32993;
const NEGATIVE: i32 = 53343;

struct Scene {
    db: crate::db::Db,
    world: Arc<crate::space::World>,
    fixture: crate::services::tests::fixture::ShipFixture,
    modules: [(i64, i32, i32); 2],
    drone: i64,
    cargo: i64,
    target: i64,
    client: crate::client::Client,
    notes: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>,
    system: i64,
}

async fn scene(firework: i64, snowball: i64, cargo: i64, duration_ms: f64) -> Scene {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = sde.map.stations().find_map(|row| row.solar_system_id).expect("system");
    let hull = sde.types.iter().find(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_SHIP)
        && row.attribute(ATTR_STRUCTURE_HP).is_some_and(|value| value > 0.0)).expect("hull").type_id;
    let drone_type = sde.types.iter().find(|row| row.published == Some(true) && row.category_id == Some(CATEGORY_DRONE)
        && row.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|value| value > 0.0)).expect("drone").type_id;
    let fixture = create_ship(&db, hull, ShipLocation::Space { system }, &[
        ShipItemSpec { type_id: FESTIVAL, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: DISPLAY, flag_id: FLAG_HI_SLOT_FIRST + 1, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: drone_type, flag_id: FLAG_DRONE_BAY, quantity: 1, singleton: true, online: false, attributes: &[] },
        ShipItemSpec { type_id: NEGATIVE, flag_id: FLAG_CARGO, quantity: cargo, singleton: false, online: false, attributes: &[] },
    ]).await.unwrap();
    for (attribute, value) in [(ATTR_CAPACITOR_CAPACITY, 1e9), (ATTR_RECHARGE_RATE, 1e6), (ATTR_CPU_OUTPUT, 1e9), (ATTR_POWER_OUTPUT, 1e9), (ATTR_DRONE_BANDWIDTH, 1e6), (ATTR_DRONE_CAPACITY, 1e6), (ATTR_MAX_TARGET_RANGE, 1e9), (ATTR_MAX_LOCKED_TARGETS, 8.0), (ATTR_SCAN_RESOLUTION, 1e6)] {
        crate::dao::items::dogma::set_override(&db, fixture.ship, attribute, value).await.unwrap();
    }
    let duration = sde.dogma.effect(EFFECT_USE_MISSILES).and_then(|row| row.duration_attribute_id).expect("useMissiles duration attribute");
    crate::dao::items::dogma::set_override(&db, fixture.items[0], duration, duration_ms).await.unwrap();
    crate::dao::items::dogma::set_override(&db, fixture.items[1], duration, duration_ms).await.unwrap();
    crate::dao::items::combat::apply_loads(&db, fixture.ship, &[
        (fixture.items[0], FLAG_HI_SLOT_FIRST, FIREWORK, firework, 0),
        (fixture.items[1], FLAG_HI_SLOT_FIRST + 1, NEGATIVE, snowball, 0),
    ]).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    client.session.apply(vec![("userid", Value::Int(fixture.account)), ("role", Value::Int(0)), ("corpid", Value::Int(1)), ("charid", Value::Int(fixture.character)),
        ("shipid", Value::Int(fixture.ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    let drone_skills: Vec<_> = crate::services::skills::requirements::requirements(&sde, drone_type).filter(|&(_, level)| level > 0).collect();
    crate::services::skills::gm::apply(&mut client, fixture.character, crate::services::skills::gm::Change::Set(drone_skills)).await.expect("drone skills");
    let (tx, notes) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx.clone();
    client.world.register_pilot(fixture.character, tx);
    let world = client.world.clone();
    fixture::admit(&db, &world, system, fixture::ball(fixture.ship, hull, fixture.character, Some(fixture.character), 0.0)).await;
    fixture::publish_fit(&db, &world, system, fixture.character, fixture.ship).await;
    let (firework_module, snowball_module) = (fixture.items[0], fixture.items[1]);
    let ship = fixture.ship;
    let flights = world.with(system, move |s| s.with_combat_fit(ship, move |fit, _, _| {
        let read = |module| {
            let key = crate::services::fit::Key::Charge(module);
            (fit.value(key, ATTR_MAX_VELOCITY), fit.value(key, ATTR_EXPLOSION_DELAY))
        };
        (read(firework_module), read(snowball_module))
    })).await.expect("actor").expect("resident flights");
    let ((firework_velocity, firework_life), (snowball_velocity, snowball_life)) = flights;
    assert!(firework_velocity.is_finite() && firework_velocity > 0.0 && snowball_velocity.is_finite() && snowball_velocity > 0.0, "resident velocities");
    assert!(firework_life.is_finite() && firework_life > 2_000.0 && snowball_life.is_finite() && snowball_life > 2_000.0, "resident lifetimes outlive one destiny step");
    let fastest = firework_velocity.max(snowball_velocity);
    let firework_radius = sde.types.get(FIREWORK).and_then(|row| row.radius).unwrap_or(1.0) as f32;
    let snowball_radius = sde.types.get(NEGATIVE).and_then(|row| row.radius).unwrap_or(1.0) as f32;
    let projectile = f64::from(firework_radius.max(snowball_radius));
    let target_radius = f64::from(fixture::ball(0, hull, fixture.character, None, 0.0).radius);
    let surface = fastest * 1.5;
    let center = surface + projectile + target_radius;
    assert!(surface / fastest * 1_000.0 > 1_000.0, "smallest surface spans one destiny step");
    assert!(center / firework_velocity * 1_000.0 < firework_life, "firework center time precedes its lifetime");
    assert!(center / snowball_velocity * 1_000.0 < snowball_life, "snowball center time precedes its lifetime");
    let target: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,$3,0,1,true,$4,0,0) RETURNING item_id", &[&hull, &fixture.character, &system, &center]).await.unwrap().get("item_id");
    crate::dao::items::dogma::set_override(&db, target, ATTR_HULL_DAMAGE, 25.0).await.unwrap();
    fixture::admit(&db, &world, system, fixture::ball(target, hull, fixture.character, None, center)).await;
    let sde = world.sde().clone();
    world.with(system, move |s| s.install_condition(target, crate::services::combat::groups_health::Condition {
        now: [1.0, 1.0, 75.0], stats: crate::services::combat::groups_health::ShipStats { max: [1.0, 1.0, 100.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 0.0 }, shield_at: 0,
    }, crate::services::fit::Fit::for_space_type(sde, hull, Vec::new()))).await.expect("target condition");
    dogma_call(&mut client, "AddTarget", &[Value::Int(target)]).await.ok().expect("target request");
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !crate::services::combat::targeting_charges::has_target(fixture.character, target) { assert!(world.after(system, 100).await, "actor stopped before the lock"); }
    }).await.expect("target lock");
    Scene { modules: [(fixture.items[0], FESTIVAL, FIREWORK), (fixture.items[1], DISPLAY, NEGATIVE)], drone: fixture.items[2], cargo: fixture.items[3], target, db, world, fixture, client, notes, system }
}

async fn close(mut scene: Scene) {
    crate::services::combat::reset(&scene.client);
    let _ = scene.world.after(scene.system, 200).await;
    scene.world.remove_ball(scene.system, scene.drone).await;
    scene.world.remove_ball(scene.system, scene.target).await;
    let owned = crate::dao::items::loading::load_items(&scene.db, crate::dao::items::loading::ItemQuery::Owner(scene.fixture.character)).await.expect("owned rows");
    scene.fixture.items.extend(owned.iter().map(|row| row.item_id));
    scene.fixture.items.sort_unstable();
    scene.fixture.items.dedup();
    cleanup_ship(&scene.db, Some(&*scene.world), scene.fixture).await.unwrap();
}
async fn finish(scene: Scene, result: std::thread::Result<()>) {
    let cleanup = crate::tests::catch_scenario(close(scene)).await;
    if let Err(panic) = result {
        if let Err(cleanup) = cleanup { tracing::error!("scene cleanup also panicked: {cleanup:?}"); }
        std::panic::resume_unwind(panic);
    }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
}

fn effect_name(sde: &Sde) -> String { sde.dogma.effect(EFFECT_USE_MISSILES).and_then(|row| row.name.clone()).expect("useMissiles") }

async fn activate(scene: &mut Scene, module: i64, repeat: i64) -> crate::services::CallResult {
    let effect = effect_name(scene.world.sde());
    dogma_call(&mut scene.client, "Activate", &[Value::Int(module), Value::str(&effect), Value::Int(scene.target), Value::Int(repeat)]).await
}

async fn actor<R: Send + 'static>(scene: &Scene, read: impl FnOnce(&mut crate::space::System) -> R + Send + 'static) -> R {
    scene.world.with(scene.system, read).await.expect("actor stopped")
}

fn module_of(scene: &Scene, index: usize) -> Module {
    let (id, type_id, _) = scene.modules[index];
    Module { id, type_id, flag: FLAG_HI_SLOT_FIRST + index as i32 }
}

fn launched(ball: &crate::space::Ball, launcher: i64, ship: i64) -> bool {
    let modules = ball.slim.iter().any(|(key, value)| *key == "launchModules" && value.items().is_some_and(|items| items.iter().any(|item| item.as_int() == Some(launcher))));
    let source = ball.slim.iter().any(|(key, value)| *key == "sourceShipID" && value.as_int() == Some(ship));
    modules && source
}

async fn quantity(scene: &Scene, module: i64) -> Option<(i32, i64)> {
    let ship = scene.fixture.ship;
    actor(scene, move |s| s.loaded_effective(ship, module).map(|(_, type_id, quantity, pending)| (type_id, quantity, pending))).await
        .filter(|(_, _, pending)| !pending).map(|(type_id, quantity, _)| (type_id, quantity))
}

async fn charge_node(scene: &Scene, module: i64) -> Option<(u64, i32)> {
    let ship = scene.fixture.ship;
    actor(scene, move |s| s.with_combat_fit(ship, move |fit, _, _| fit.charge_incarnation(module))).await.flatten()
}

async fn flights(scene: &Scene, launcher: i64) -> Vec<(i64, i32, i64, bool)> {
    let ship = scene.fixture.ship;
    actor(scene, move |s| s.balls().filter_map(|ball| launched(ball, launcher, ship).then_some((ball.id, ball.type_id, ball.owner_id, s.projectile_damageable(ball.id)))).collect()).await
}

async fn paid_flight(scene: &Scene, module: i64, charge: i32, quantity: i64, except: Option<i64>) -> (i64, i32, i64, bool) {
    let (world, system, ship) = (scene.world.clone(), scene.system, scene.fixture.ship);
    match tokio::time::timeout(std::time::Duration::from_secs(30), async move {
        loop {
            let seen = world.with(system, move |s| {
                let paid = s.loaded_effective(ship, module).is_some_and(|(_, type_id, left, pending)| type_id == charge && left == quantity && !pending);
                let flight = s.balls().find_map(|ball| (launched(ball, module, ship) && ball.type_id == charge && s.projectile_damageable(ball.id) && Some(ball.id) != except).then_some((ball.id, ball.type_id, ball.owner_id, true)));
                (paid, flight)
            }).await.expect("actor stopped before the accepted shot");
            if let (true, Some(flight)) = seen { return flight; }
            assert!(world.after(system, 100).await, "actor stopped before the accepted shot");
        }
    }).await {
        Ok(flight) => flight,
        Err(_) => panic!("paid flight timed out module {module} charge {charge} expected {quantity} except {except:?} loaded {:?} flights {:?}", self::quantity(scene, module).await, flights(scene, module).await),
    }
}

async fn flight_gone(scene: &Scene, id: i64, timeout: std::time::Duration) -> Result<(), tokio::time::error::Elapsed> {
    let (world, system) = (scene.world.clone(), scene.system);
    tokio::time::timeout(timeout, async move {
        loop {
            if !world.with(system, move |s| s.ball(id).is_some()).await.expect("actor stopped before flight removal") { return; }
            assert!(world.after(system, 100).await, "actor stopped before flight removal");
        }
    }).await
}

async fn now_ms(scene: &Scene) -> i64 { actor(scene, |s| s.now_ms()).await }

async fn lifetime_ms(scene: &Scene, module: i64) -> f64 {
    let ship = scene.fixture.ship;
    actor(scene, move |s| s.with_combat_fit(ship, move |fit, _, _| fit.value(crate::services::fit::Key::Charge(module), ATTR_EXPLOSION_DELAY))).await.expect("charge lifetime")
}

async fn condition(scene: &Scene) -> (f64, f64, f64) {
    let target = scene.target;
    actor(scene, move |s| s.ship_condition(target)).await.expect("target condition")
}

fn events(scene: &mut Scene) -> Vec<(String, Vec<Value>)> { std::iter::from_fn(|| scene.notes.try_recv().ok()).collect() }

fn effect_row<'a>(events: &'a [(String, Vec<Value>)], module: i64, start: i64) -> Option<&'a [Value]> {
    events.iter().find_map(|(event, args)| {
        let environment = args.get(5)?.items()?;
        (event == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module))
            && args.get(1) == Some(&Value::Int(i64::from(EFFECT_USE_MISSILES)))
            && args.get(3) == Some(&Value::Int(start)) && args.get(4) == Some(&Value::Int(start))
            && environment.first() == Some(&Value::Int(module))
            && environment.get(6) == Some(&Value::Int(i64::from(EFFECT_USE_MISSILES))))
            .then_some(environment.as_ref())
    })
}

fn generation(module: i64) -> Arc<AtomicBool> {
    let st = state();
    let active = st.active.get(&module).expect("activation current").stop.clone();
    let generation = st.activation_generation.get(&module).expect("activation generation").clone();
    assert!(Arc::ptr_eq(&active, &generation), "active stop and generation differ");
    generation
}
async fn activation_retired(scene: &Scene, module: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let running = state().active.contains_key(&module);
            if !running { return; }
            assert!(scene.world.after(scene.system, 100).await, "actor stopped before the activation retired");
        }
    }).await.expect("activation retired");
}

async fn launch_drone(scene: &mut Scene) {
    let args = [Value::list([Value::tuple([Value::Int(scene.drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let kwargs = Value::None;
    let bind = Value::tuple([Value::Int(scene.system), Value::Int(5)]);
    let launched = crate::services::ship::dispatch(&mut scene.client, &crate::services::Call { service: "ship", method: "LaunchDrones", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await.expect("drone dispatch").expect("drone launch");
    let Value::Dict(rows) = &launched else { panic!("launch mapping {launched:?}") };
    let [(Value::Int(requested), Value::List(ids))] = rows.as_slice() else { panic!("drone launch refused: {launched:?}") };
    assert!(*requested == scene.drone && ids == &[Value::Int(scene.drone)]);
}

async fn drone_idle(scene: &Scene) -> bool {
    let drone = scene.drone;
    actor(scene, move |s| s.drone(drone).map(|drone| drone.order)).await == Some(crate::space::drone::DroneOrder::Idle)
}

async fn dirty(scene: &Scene, module: i64) -> Option<(i32, i64)> {
    let ship = scene.fixture.ship;
    actor(scene, move |s| s.capture_dirty_loaded(&[ship]).into_iter().find(|row| row.module == module).map(|row| (row.type_id, row.quantity))).await
}
async fn cargo_quantity(scene: &Scene, type_id: i32) -> i64 {
    let rows = crate::dao::items::loading::load_items(&scene.db, crate::dao::items::loading::ItemQuery::LocationFlagTypeOwnerStack(scene.fixture.ship, FLAG_CARGO, type_id, scene.fixture.character)).await.unwrap();
    rows.iter().map(|row| row.quantity).sum()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn festival_launch_pays_flies_and_leaves_no_combat() {
    let mut scene = scene(2, 2, 1, 250.0).await;
    let result = crate::tests::catch_scenario(async {
        let security = crate::services::crimewatch::security_status(&scene.db, scene.fixture.character).await.unwrap();
        let before = condition(&scene).await;
        assert!(before.2 > 0.0, "target starts below full hull");
        let (module, _launcher, charge) = scene.modules[0];
        let launched_at = now_ms(&scene).await;
        let life = lifetime_ms(&scene, module).await;
        assert!(life.is_finite() && life > 0.0, "charge lifetime");
        assert_eq!(activate(&mut scene, module, 2).await.expect("activate"), Value::None);
        let first = paid_flight(&scene, module, charge, 1, None).await;
        assert_eq!((first.1, first.2, first.3), (charge, scene.fixture.character, true));
        let second = paid_flight(&scene, module, charge, 0, Some(first.0)).await;
        assert_ne!(first.0, second.0, "the second shot is a distinct flight");
        flight_gone(&scene, first.0, std::time::Duration::from_secs(30)).await.expect("first firework reaches its contact terminal");
        flight_gone(&scene, second.0, std::time::Duration::from_secs(30)).await.expect("second firework reaches its contact terminal");
        let removed_at = now_ms(&scene).await;
        assert!(removed_at < launched_at + life.ceil() as i64, "removal happened before the prelaunch lifetime, so it is contact rather than expiry");
        assert_eq!(quantity(&scene, module).await, Some((charge, 0)), "the emptied slot keeps its type");
        assert!(charge_node(&scene, module).await.is_none(), "the last unit removes the resident charge node");
        assert!(activate(&mut scene, module, 0).await.is_err(), "empty launcher refuses a third shot");
        assert!(flights(&scene, module).await.is_empty(), "the refusal inserts no projectile");
        let (display, _type_id, snowball) = scene.modules[1];
        let snow_at = now_ms(&scene).await;
        let snow_life = lifetime_ms(&scene, display).await;
        assert_eq!(activate(&mut scene, display, 0).await.expect("display activate"), Value::None);
        let snow = paid_flight(&scene, display, snowball, 1, None).await;
        assert_eq!((snow.1, snow.2, snow.3), (snowball, scene.fixture.character, true));
        flight_gone(&scene, snow.0, std::time::Duration::from_secs(30)).await.expect("snowball reaches its contact terminal");
        assert!(now_ms(&scene).await < snow_at + snow_life.ceil() as i64, "snowball removal is inside its prelaunch lifetime");
        assert!(!actor(&scene, move |s| s.projectile_damageable(snow.0)).await);
        assert_eq!(condition(&scene).await, before, "contact neither damages nor repairs the nonfull target");
        activation_retired(&scene, display).await;
        let inert_at = now_ms(&scene).await;
        let inert_life = lifetime_ms(&scene, display).await;
        assert!(inert_life.is_finite() && inert_life > 0.0, "untargeted charge lifetime");
        assert_eq!(activate(&mut scene, display, 0).await.expect("second display shot"), Value::None);
        let inert = paid_flight(&scene, display, snowball, 0, Some(snow.0)).await;
        actor(&scene, move |s| s.invalidate_missile_target(scene.target)).await;
        assert!(actor(&scene, move |s| s.ball(inert.0).is_some() && !s.projectile_damageable(inert.0)).await, "invalidation consumes the profile and keeps the visual");
        flight_gone(&scene, inert.0, std::time::Duration::from_millis(inert_life.ceil() as u64) + std::time::Duration::from_secs(30)).await.expect("untargeted visual reaches its expiry terminal");
        assert!(now_ms(&scene).await >= inert_at + inert_life.ceil() as i64, "untargeted removal is not earlier than its prelaunch lifetime");
        assert_eq!(condition(&scene).await, before, "invalidation and expiry leave the damaged target unchanged");
        assert!(activate(&mut scene, display, 0).await.is_err(), "empty launcher refuses another shot");
        assert_eq!(quantity(&scene, display).await, Some((snowball, 0)));
        assert!(charge_node(&scene, display).await.is_none(), "the last unit removed the resident charge node");
        let seen = events(&mut scene);
        let started = effect_row(&seen, module, 1).expect("effect 101 start");
        assert_eq!(started.get(2), Some(&Value::Int(scene.fixture.ship)));
        assert_eq!(started.get(3), Some(&Value::Int(scene.target)));
        let stopped = effect_row(&seen, module, 0).expect("effect 101 stop");
        assert_eq!(stopped.get(2), Some(&Value::Int(scene.fixture.ship)));
        assert!(seen.iter().all(|(event, _)| event != "OnDamageMessage"));
        let facts = actor(&scene, |s| crate::space::snapshot::capture_fact_checkpoint(s)).await;
        assert!(facts.expect("fact capture").is_none(), "accepted shots publish no combat fact");
        assert!(crate::services::crimewatch::weapons_timer(scene.fixture.character).is_none());
        assert_eq!(crate::services::crimewatch::security_status(&scene.db, scene.fixture.character).await.unwrap(), security);
    }).await;
    finish(scene, result).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn festival_held_permit_stop_reload_drone_and_dirty_boundary() {
    let mut scene = scene(4, 2, 4, 15_000.0).await;
    let result = crate::tests::catch_scenario(async {
        let (module, _launcher, charge) = scene.modules[0];
        launch_drone(&mut scene).await;
        assert!(drone_idle(&scene).await, "drone is idle before the shot");
        assert_eq!(activate(&mut scene, module, 0).await.expect("activate"), Value::None);
        let paid = paid_flight(&scene, module, charge, 3, None).await;
        assert!(drone_idle(&scene).await, "drone stays idle after the accepted payment");
        let old_token = actor(&scene, move |s| s.fitted_charge_token(scene.fixture.ship, module)).await.expect("resident token");
        let admitted = actor(&scene, {
            let module = module_of(&scene, 0);
            let ship = scene.fixture.ship;
            let owner = scene.fixture.character;
            move |s| {
                let effect = crate::services::effects::registry::resolve(s.sde().expect("sde"), EFFECT_USE_MISSILES).expect("useMissiles");
                let (admission, type_id) = admit(s, ship, owner, &module, &effect).expect("admit").expect("festival admission");
                let flight = s.with_combat_fit(ship, |fit, _, _| admission.capture(fit, &module, Some(admission.token))).expect("fit").expect("capture");
                let permit = s.hold_loaded_use(ship, module.id).expect("finite permit");
                (admission, type_id, flight, permit)
            }
        }).await;
        assert_eq!(admitted.1, charge, "admission names the resident charge");
        assert!(same_charge(admitted.0.token, old_token), "admission keeps the resident token");
        let admitted_membership = admitted.0.token.membership;
        let stopped_generation = generation(module);
        let before_flights = flights(&scene, module).await;
        assert!(before_flights.iter().any(|row| row.0 == paid.0 && row.3), "the paid flight is still live");
        let effect = effect_name(scene.world.sde());
        dogma_call(&mut scene.client, "Deactivate", &[Value::Int(module), Value::str(&effect)]).await.expect("deactivate");
        assert!(stopped_generation.load(Ordering::Relaxed), "real deactivate set the active generation");
        let refused = actor(&scene, {
            let (admission, _, flight, permit) = admitted;
            let module = module_of(&scene, 0);
            let ship = scene.fixture.ship;
            let owner = scene.fixture.character;
            let generation = stopped_generation.clone();
            let sde = scene.world.sde().clone();
            let current = actor(&scene, move |s| s.combat_membership_generation(ship)).await;
            assert_eq!(current, Some(admitted_membership), "the admitted membership is still current");
            let target = scene.target;
            move |s| launch_in_actor(s, &sde, ship, owner, Some(admitted_membership), &module, i64::from(EFFECT_USE_MISSILES), &generation, generation.load(Ordering::Relaxed), target, admission, flight, Some(permit), Some(old_token))
        }).await;
        assert!(refused.is_none(), "stopped generation releases the permit and inserts nothing");
        assert_eq!(flights(&scene, module).await, before_flights, "deactivate does not despawn the paid flight or insert another");
        assert_eq!(quantity(&scene, module).await, Some((charge, 3)));
        assert_eq!(charge_node(&scene, module).await.map(|node| node.1), Some(charge));
        flight_gone(&scene, paid.0, std::time::Duration::from_secs(30)).await.expect("held permit leaves the accepted flight to its own terminal");
        assert!(flights(&scene, module).await.is_empty(), "the original flight reaches its own terminal");
        assert!(drone_idle(&scene).await, "drone stays idle through the terminal");
        let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
        actor(&scene, move |s| s.drain_loaded(done_tx)).await;
        tokio::time::timeout(std::time::Duration::from_secs(10), done_rx.recv()).await.expect("drain").expect("finite uses released");
        activation_retired(&scene, module).await;
        assert_eq!(activate(&mut scene, module, 0).await.expect("live activate"), Value::None);
        let live = paid_flight(&scene, module, charge, 2, None).await;
        let live_token = actor(&scene, move |s| s.fitted_charge_token(scene.fixture.ship, module)).await.expect("live token");
        let live_admitted = actor(&scene, {
            let module = module_of(&scene, 0);
            let ship = scene.fixture.ship;
            let owner = scene.fixture.character;
            move |s| {
                let effect = crate::services::effects::registry::resolve(s.sde().expect("sde"), EFFECT_USE_MISSILES).expect("useMissiles");
                admit(s, ship, owner, &module, &effect).expect("admit").expect("live admission")
            }
        }).await;
        assert_eq!(live_admitted.1, charge);
        assert!(same_charge(live_admitted.0.token, live_token));
        let live_generation = generation(module);
        assert!(!live_generation.load(Ordering::Relaxed), "the live generation is not stopped");
        let reload_ms = actor(&scene, {
            let ship = scene.fixture.ship;
            move |s| s.with_combat_fit(ship, move |fit, _, _| fit.value(crate::services::fit::Key::Module(module), infinity_core::consts::dogma::ATTR_RELOAD_TIME))
        }).await.expect("reload time");
        assert!(reload_ms <= 10_000.0, "the live cycle outlasts the effective reload");
        crate::services::combat::targeting_charges::load_ammo_core(&mut scene.client, scene.fixture.ship, vec![module], vec![scene.cargo], Some(scene.fixture.ship)).await.expect("reload");
        assert!(Arc::ptr_eq(&generation(module), &live_generation), "reload leaves the live generation current");
        assert!(!live_generation.load(Ordering::Relaxed), "reload does not stop the live generation");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let token = actor(&scene, move |s| s.fitted_charge_token(scene.fixture.ship, module)).await;
                if token.is_some_and(|token| token.type_id == NEGATIVE && token.incarnation != live_token.incarnation) { return; }
                assert!(scene.world.after(scene.system, 100).await, "actor stopped before the reload");
            }
        }).await.expect("successor publication");
        assert!(Arc::ptr_eq(&generation(module), &live_generation), "the live generation survives publication");
        assert!(!live_generation.load(Ordering::Relaxed), "publication does not stop the live generation");
        assert!(flights(&scene, module).await.iter().all(|row| row.1 != NEGATIVE), "the old activation inserts no successor flight");
        let refused_capture = actor(&scene, {
            let admission = live_admitted.0;
            let module = module_of(&scene, 0);
            let ship = scene.fixture.ship;
            move |s| s.with_combat_fit(ship, move |fit, _, stamp| {
                let successor = fit.charge_incarnation(module.id).map(|(incarnation, type_id)| ChargeToken { ship, membership: stamp.membership, module: module.id, incarnation, type_id });
                admission.capture(fit, &module, successor)
            })
        }).await.expect("fit");
        assert!(matches!(refused_capture, Err(crate::services::CallError::User { .. })), "the live admission refuses the successor");
        let wrong_paid = actor(&scene, {
            let ship = scene.fixture.ship;
            move |s| {
                let permit = s.hold_loaded_use(ship, module).expect("successor permit");
                consume_charge_in_actor(s, Some(permit), Some(live_token))
            }
        }).await;
        assert!(wrong_paid.is_none(), "the live token cannot debit the successor");
        assert_eq!(quantity(&scene, module).await, Some((NEGATIVE, 4)), "reload published the owned cargo stack");
        dogma_call(&mut scene.client, "Deactivate", &[Value::Int(module), Value::str(&effect)]).await.expect("stop the live activation");
        activation_retired(&scene, module).await;
        flight_gone(&scene, live.0, std::time::Duration::from_secs(30)).await.expect("stopped activation leaves the accepted flight to its own terminal");
        let fresh_before = 4;
        assert_eq!(activate(&mut scene, module, 0).await.expect("fresh activate"), Value::None);
        let fresh = paid_flight(&scene, module, NEGATIVE, fresh_before - 1, None).await;
        assert_eq!(fresh.1, NEGATIVE);
        flight_gone(&scene, fresh.0, std::time::Duration::from_secs(30)).await.expect("fresh activation leaves the accepted flight to its own terminal");
        let remainder = quantity(&scene, module).await.expect("remaining dirty").1;
        assert!(remainder > 0, "a positive remainder stays dirty");
        let dirty_row = dirty(&scene, module).await.expect("dirty absolute");
        assert_eq!(dirty_row, (NEGATIVE, remainder));
        scene.world.flush_positions().await.expect("dirty flush");
        assert!(dirty(&scene, module).await.is_none(), "flush clears the matched row");
        assert_eq!(crate::services::combat::targeting_charges::loaded(&scene.db, scene.fixture.ship, FLAG_HI_SLOT_FIRST).await.unwrap(), Some((NEGATIVE, remainder)));
        activation_retired(&scene, module).await;
        assert_eq!(activate(&mut scene, module, 0).await.expect("new dirty shot"), Value::None);
        let again = paid_flight(&scene, module, NEGATIVE, remainder - 1, Some(fresh.0)).await;
        let unsettled = dirty(&scene, module).await.expect("new positive dirty remainder");
        assert!(unsettled.1 > 0, "the business settlement starts from a positive dirty quantity");
        assert_eq!(unsettled, (NEGATIVE, remainder - 1));
        let cargo_before = cargo_quantity(&scene, NEGATIVE).await;
        crate::services::combat::targeting_charges::unload_ammo_core(&mut scene.client, scene.fixture.ship, vec![module], scene.fixture.ship, FLAG_CARGO).await.expect("unload settles the dirty remainder");
        assert_eq!(cargo_quantity(&scene, NEGATIVE).await - cargo_before, unsettled.1, "cargo gained the unsettled quantity");
        assert_eq!(quantity(&scene, module).await, None);
        assert!(dirty(&scene, module).await.is_none());
        assert_eq!(crate::services::combat::targeting_charges::loaded(&scene.db, scene.fixture.ship, FLAG_HI_SLOT_FIRST).await.unwrap(), None);
        assert!(flights(&scene, module).await.iter().all(|row| row.0 == again.0 || !row.3));
    }).await;
    finish(scene, result).await;
}