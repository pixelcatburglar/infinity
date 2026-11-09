//! Real overloaded propulsion cycles, durable burnout and station repair.
use super::fixture;
mod cleanup;
mod banks;
mod skills;
use infinity_core::{
    consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CAPACITOR_NEED, dogma::ATTR_CPU, dogma::ATTR_CPU_OUTPUT,
        dogma::ATTR_HI_SLOTS, dogma::ATTR_HULL_DAMAGE, dogma::ATTR_LOW_SLOTS, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_MED_SLOTS,
        dogma::ATTR_POWER, dogma::ATTR_POWER_OUTPUT, dogma::ATTR_REQUIRED_SKILL_LEVELS, dogma::ATTR_RIG_SLOTS,
        dogma::ATTR_STRUCTURE_HP, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_MICROWARPDRIVE,
        FLAG_HANGAR, FLAG_MED_SLOT_FIRST},
};
use crate::{
    client::Client,
    config::Config,
    conn::Conn,
    dao::items::dogma,
    handshake::Login,
    services::{self, combat, effects::{heat, online, registry::{self, EffectAttribute}}, fit::{Fit, Key}, wallets, Call, CallResult},
    space::World,
};
use marshal::Value;
use std::{sync::Arc, time::Duration};

/// SDE overloadSpeed effect and heat attributes.
const OVERLOAD_SPEED: i32 = 3175;
const HEAT_ABSORPTION: i32 = 1180;
const HEAT_DAMAGE: i32 = 1211;
const REQUIRED_THERMODYNAMICS: i32 = 1212;
const HEAT_GENERATION: i32 = 1224;
const MEDIUM_CAPACITY: i32 = 1199;
const SYSTEM: i64 = 1;

async fn call(c: &mut Client, service: &str, method: &str, args: &[Value], bind: Option<&Value>) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    services::dispatch(c, &Call { service, method, args, kwargs: &kwargs, bound: true, bind_params: bind }).await
}

async fn assert_cached_defenses(db: &crate::db::Db, world: &World, owner: i64, ship: i64) -> [f64; 2] {
    let fit = Fit::load_in_space(db, world, SYSTEM, owner, ship).await.ok().expect("load expected fit");
    let expected = combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
    let snapshot = world.with(SYSTEM, move |s| s.combat_condition_snapshot(ship)).await.flatten().expect("actor condition");
    assert_eq!(snapshot.condition.stats.max, expected.max);
    assert_eq!(snapshot.condition.stats.resonance, expected.resonance);
    assert_eq!(snapshot.condition.stats.shield_tau_ms, expected.shield_tau_ms);
    [1, 2].map(|i| snapshot.condition.stats.max[i] - snapshot.condition.now[i])
}
async fn seed_module_prerequisites(db: &crate::db::Db, sde: &crate::sde::Sde, character: i64, module: i32) {
    for (skill, level) in services::skills::requirements::requirements(sde, module) {
        let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("module prerequisite rank");
        let points = services::skills::formulas::sp_for_level(rank, i64::from(level));
        crate::dao::skills::training::set_level(db, character, skill, level, points).await.unwrap();
    }
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn overloaded_cycle_burns_persists_and_station_repairs() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(MEDIUM_CAPACITY).is_some_and(|v| v > 0.0)).expect("published heat-capable hull").type_id;
    let module = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.base_price.is_some_and(|v| v >= f64::from(t.portion_size.unwrap_or(1).max(1)))
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
        && t.effects().iter().any(|e| e.effect_id == OVERLOAD_SPEED)).expect("priced overloadable propulsion module").type_id;
    let active = registry::resolve(&sde, EFFECT_MICROWARPDRIVE).unwrap();
    let duration = active.attribute(EffectAttribute::Duration).expect("propulsion duration attribute");
    let effect_name = sde.dogma.effect(EFFECT_MICROWARPDRIVE).unwrap().name.clone().unwrap();
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_MED_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        fixture::position(&d, f.ship, SYSTEM, [0.0; 3]).await.unwrap();
        for (item, attr, value) in [
            (f.ship, ATTR_LOW_SLOTS, 0.0), (f.ship, ATTR_HI_SLOTS, 0.0),
            (f.ship, ATTR_MED_SLOTS, 1.0), (f.ship, ATTR_RIG_SLOTS, 0.0),
            (f.ship, ATTR_CPU_OUTPUT, 1_000_000.0), (f.ship, ATTR_POWER_OUTPUT, 1_000_000.0),
            (f.ship, ATTR_CAPACITOR_CAPACITY, 1_000_000.0), (f.ship, HEAT_GENERATION, 1000.0),
            (f.module, ATTR_CPU, 0.0), (f.module, ATTR_POWER, 0.0),
            (f.module, ATTR_CAPACITOR_NEED, 0.0), (f.module, REQUIRED_THERMODYNAMICS, 0.0),
            (f.module, HEAT_ABSORPTION, 1000.0), (f.module, HEAT_DAMAGE, 1_000_000.0),
            (f.module, duration, 2000.0),
        ] { dogma::set_override(&d, item, attr, value).await.unwrap(); }
        for attr in ATTR_REQUIRED_SKILL_LEVELS { dogma::set_override(&d, f.module, attr, 0.0).await.unwrap(); }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, addr) = listener.accept().await.unwrap();
        let login = Login { user_id: f.account, user_name: f.account.to_string(), role: 0, client_id: f.character, session_id: f.character, language_id: "EN".into() };
        let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), d.clone(), sde.clone(), crate::chat::Hub::new("localhost", d.clone(), w.clone()), w.clone(), login);
        c.session.apply(vec![("charid", Value::Int(f.character)), ("shipid", Value::Int(f.ship)), ("solarsystemid", Value::Int(SYSTEM))]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = tx.clone();
        w.register_pilot(f.character, tx);
        w.system(SYSTEM).await.unwrap();
        let market = services::market::lock().await;
        let hull_row = crate::dao::items::space_ships::FreeShip { item_id: f.ship, type_id: hull, owner_id: f.character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = services::ship::balls::piloted(&d, sde.clone(), &hull_row, f.character, 0.0).await.ok().expect("prepare ship");
        w.with(SYSTEM, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        drop(market);
        assert_cached_defenses(&d, &w, f.character, f.ship).await;
        combat::groups_health::set_damage(&c, SYSTEM, f.ship, f.character, [false, true, true], 0.25).await.ok().expect("damage armor and hull");
        let raw_damage = assert_cached_defenses(&d, &w, f.character, f.ship).await;
        let baseline = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
        let bad = [Value::Int(f.module), Value::Int(i64::from(OVERLOAD_SPEED + 1))];
        assert!(call(&mut c, "dogmaIM", "Overload", &bad, None).await.is_err());
        assert!(!heat::overloaded_modules().contains(&f.module));
        let overload = [Value::Int(f.module), Value::Int(i64::from(OVERLOAD_SPEED))];
        dogma::set_override(&d, f.module, REQUIRED_THERMODYNAMICS, 1.0).await.unwrap();
        fixture::publish_fit(&d, &w, SYSTEM, f.character, f.ship).await;
        assert!(call(&mut c, "dogmaIM", "Overload", &overload, None).await.is_err());
        assert!(!heat::overloaded_modules().contains(&f.module));
        // Reconciliation checks SDE prerequisites, not fitted level overrides.
        seed_module_prerequisites(&d, &sde, f.character, module).await;
        dogma::set_override(&d, f.module, REQUIRED_THERMODYNAMICS, 0.0).await.unwrap();
        dogma::set_override(&d, f.module, HEAT_DAMAGE, 0.0).await.unwrap();
        fixture::publish_fit(&d, &w, SYSTEM, f.character, f.ship).await;
        let running = [Value::Int(f.module), Value::str(&effect_name), Value::None, Value::Int(5)];
        let kwargs = Value::Dict(vec![]);
        let running_call = Call { service: "dogmaIM", method: "Activate", args: &running, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dogma::dispatch(&mut c, &running_call).await.ok().expect("activate unheated propulsion");
        let normal_speed = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap().value(Key::Ship, ATTR_MAX_VELOCITY);
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw_damage);
        assert!(call(&mut c, "dogmaIM", "Overload", &overload, None).await.is_ok());
        assert!(!heat::overloaded_modules().contains(&f.module));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !heat::overloaded_modules().contains(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("pending start boundary");
        let hot_speed = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap().value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!(hot_speed > normal_speed);
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw_damage);
        assert!(call(&mut c, "dogmaIM", "StopOverload", &overload, None).await.is_ok());
        assert!(heat::overloaded_modules().contains(&f.module));
        tokio::time::timeout(Duration::from_secs(10), async {
            while heat::overloaded_modules().contains(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("pending stop boundary");
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw_damage);
        let stop_args = [Value::Int(f.module), Value::str(&effect_name)];
        let stop = Call { service: "dogmaIM", method: "Deactivate", args: &stop_args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dogma::dispatch(&mut c, &stop).await.ok().expect("stop propulsion");
        tokio::time::timeout(Duration::from_secs(10), async {
            while combat::state().active.contains_key(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("propulsion retired");
        assert_eq!(assert_cached_defenses(&d, &w, f.character, f.ship).await, raw_damage);
        while rx.try_recv().is_ok() {}
        dogma::set_override(&d, f.module, HEAT_DAMAGE, 0.0).await.unwrap();
        fixture::publish_fit(&d, &w, SYSTEM, f.character, f.ship).await;
        assert!(call(&mut c, "dogmaIM", "Overload", &overload, None).await.is_ok());
        assert!(heat::overloaded_modules().contains(&f.module));
        let prime = heat::ship_state(&w, SYSTEM, f.ship).await;
        assert!(prime.as_ref().is_none_or(|racks| racks.iter().all(|r| r.incoming == 0.0)));
        let activate = [Value::Int(f.module), Value::str(&effect_name), Value::None, Value::Int(100)];
        let kwargs = Value::Dict(vec![]);
        let activation = Call { service: "dogmaIM", method: "Activate", args: &activate, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dogma::dispatch(&mut c, &activation).await.ok().expect("activate propulsion");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if heat::ship_state(&w, SYSTEM, f.ship).await.is_some_and(|r| r.iter().any(|r| r.incoming > 0.0)) { break; }
                assert!(w.after(SYSTEM, 100).await);
            }
        }).await.expect("heat source connected");
        let overloaded = Fit::load_in_space(&d, &w, SYSTEM, f.character, f.ship).await.ok().unwrap();
        assert!(overloaded.value(Key::Ship, ATTR_MAX_VELOCITY) > baseline.value(Key::Ship, ATTR_MAX_VELOCITY));
        let prime_args = [Value::Bool(false), Value::Bool(true), Value::None];
        for _ in 0..2 {
            let info = call(&mut c, "dogmaIM", "GetAllInfo", &prime_args, None).await.ok().expect("reprime");
            assert!(dogma::online(&d, f.ship).await.unwrap().iter().any(|m| m.item_id == f.module), "reprime must keep the propulsion module online");
            let state = services::value_field(&info, "shipState").unwrap().items().unwrap();
            let Value::Dict(racks) = &state[3] else { panic!("heat bootstrap mapping") };
            let (_, medium) = racks.iter().find(|(id, _)| id.as_int() == Some(i64::from(infinity_core::consts::dogma::ATTR_HEAT_MED))).unwrap();
            assert!(medium.items().unwrap()[2].as_float().unwrap() > 0.0, "medium heat row={medium:?}, actor={:?}", heat::ship_state(&w, SYSTEM, f.ship).await.map(|r| r.into_iter().map(|rack| (rack.heat, rack.incoming, rack.capacity)).collect::<Vec<_>>()));
        }
        crate::services::dogma::dispatch(&mut c, &stop).await.ok().expect("stop priming propulsion");
        tokio::time::timeout(Duration::from_secs(10), async {
            while combat::state().active.contains_key(&f.module) { assert!(w.after(SYSTEM, 100).await); }
        }).await.expect("priming propulsion retired");
        while rx.try_recv().is_ok() {}
        dogma::set_override(&d, f.module, HEAT_DAMAGE, 1_000_000.0).await.unwrap();
        fixture::publish_fit(&d, &w, SYSTEM, f.character, f.ship).await;
        let burn = [Value::Int(f.module), Value::str(&effect_name), Value::None, Value::Int(1)];
        let burn_call = Call { service: "dogmaIM", method: "Activate", args: &burn, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dogma::dispatch(&mut c, &burn_call).await.ok().expect("activate forced burnout");
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if dogma::overrides(&d, f.module).await.unwrap().iter().any(|v| v.attribute_id == ATTR_HULL_DAMAGE && v.value > 0.0)
                    && !dogma::online(&d, f.ship).await.unwrap().iter().any(|m| m.item_id == f.module)
                    && !heat::overloaded_modules().contains(&f.module) { break; }
                assert!(w.after(SYSTEM, 100).await);
            }
        }).await.expect("cycle injury persisted");
        let fit = Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap();
        let hp = fit.value(Key::Module(f.module), ATTR_STRUCTURE_HP);
        assert_eq!(fit.value(Key::Module(f.module), ATTR_HULL_DAMAGE), hp);
        assert!(!dogma::online(&d, f.ship).await.unwrap().iter().any(|m| m.item_id == f.module));
        assert!(!heat::overloaded_modules().contains(&f.module));
        let info = call(&mut c, "dogmaIM", "GetAllInfo", &prime_args, None).await.ok().expect("burnt reprime");
        let state = services::value_field(&info, "shipState").unwrap().items().unwrap();
        let Value::Dict(instances) = &state[0] else { panic!("instance bootstrap mapping") };
        let (_, module_row) = instances.iter().find(|(id, _)| id.as_int() == Some(f.module)).unwrap();
        assert_eq!(module_row.items().unwrap()[2].as_float(), Some(hp));
        assert!(online::set_online(&services::identity::Identity::of_client(&c), &c.db, &c.world, &c.sde, c.notify_tx.clone(), c.login.user_id, f.ship, f.module, true, |notification| online::dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid"))).await.is_err());
        assert!(w.after(SYSTEM, 300).await);
        let mut adds = 0;
        let mut removes = 0;
        while let Ok((name, _)) = rx.try_recv() { if name == "OnHeatAdded" { adds += 1; } if name == "OnHeatRemoved" { removes += 1; } }
        assert_eq!((adds, removes), (1, 1));
        w.remove_ball(SYSTEM, f.ship).await;
        assert!(!heat::overloaded_modules().contains(&f.module));
        let fresh_world = World::new(d.clone(), sde.clone(), 1);
        fixture::admit(&d, &fresh_world, SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        let fresh_fit = Fit::load_in_space(&d, &fresh_world, SYSTEM, f.character, f.ship).await.ok().unwrap();
        assert_eq!(fresh_fit.value(Key::Module(f.module), ATTR_HULL_DAMAGE), hp);
        assert!(heat::ship_state(&fresh_world, SYSTEM, f.ship).await.is_none_or(|r| r.iter().all(|r| r.heat == 0.0 && r.incoming == 0.0)));
        fresh_world.remove_ball(SYSTEM, f.ship).await;
        drop(fresh_world);
        let station = sde.map.stations().next().unwrap().station_id;
        d.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&f.ship, &station, &FLAG_HANGAR]).await.unwrap();
        d.execute("UPDATE game.characters SET station_id=$2, balance=1000000000000 WHERE character_id=$1", &[&f.character, &station]).await.unwrap();
        c.session.apply(vec![("stationid", Value::Int(station)), ("solarsystemid", Value::None)]);
        let bind = Value::tuple([Value::Int(station), Value::Int(station), Value::Int(0)]);
        let ids = Value::list([Value::Int(f.ship)]);
        let quote = call(&mut c, "repairSvc", "GetRepairQuotes", &[ids.clone()], Some(&bind)).await.ok().unwrap();
        let Value::Dict(groups) = quote else { panic!("quote mapping") };
        let rows = groups.iter().find(|(id, _)| *id == Value::Int(f.ship)).unwrap().1.items().unwrap();
        let row = rows.iter().find(|row| services::value_field(row, "itemID").and_then(Value::as_int) == Some(f.module)).expect("burnt module quote");
        let damage = services::value_field(row, "damage").and_then(Value::as_float).unwrap();
        let unit = services::value_field(row, "costToRepairOneUnitOfDamage").and_then(Value::as_float).unwrap();
        assert_eq!(damage, hp);
        let cost = damage.ceil() * unit;
        assert!(cost > 0.01);
        let balance = wallets::balance(&d, f.character).await.ok().unwrap();
        assert!(call(&mut c, "repairSvc", "RepairItemsInStation", &[ids.clone(), Value::Float(cost * 2.0)], Some(&bind)).await.is_err());
        assert_eq!(wallets::balance(&d, f.character).await.ok().unwrap(), balance);
        assert_eq!(Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap().value(Key::Module(f.module), ATTR_HULL_DAMAGE), hp);
        call(&mut c, "repairSvc", "RepairItemsInStation", &[ids.clone(), Value::Float(cost)], Some(&bind)).await.ok().expect("full repair");
        assert_eq!(wallets::balance(&d, f.character).await.ok().unwrap(), balance.checked_sub(crate::isk::IskAmount::from_isk(services::market::round2(cost)).unwrap()).unwrap());
        assert_eq!(Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap().value(Key::Module(f.module), ATTR_HULL_DAMAGE), 0.0);
        let repaired_balance = wallets::balance(&d, f.character).await.ok().unwrap();
        assert!(call(&mut c, "repairSvc", "RepairItemsInStation", &[ids, Value::Float(cost)], Some(&bind)).await.is_err());
        assert_eq!(wallets::balance(&d, f.character).await.ok().unwrap(), repaired_balance);
        assert_eq!(Fit::load(&d, sde.clone(), f.character, f.ship).await.ok().unwrap().value(Key::Module(f.module), ATTR_HULL_DAMAGE), 0.0);
        online::set_online(&services::identity::Identity::of_client(&c), &c.db, &c.world, &c.sde, c.notify_tx.clone(), c.login.user_id, f.ship, f.module, true, |notification| online::dispatch_notification(notification, &c.world, &c.notify_tx, c.session.int("charid"))).await.ok().expect("online after repair");
        assert!(dogma::online(&d, f.ship).await.unwrap().iter().any(|m| m.item_id == f.module));
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, std::sync::atomic::Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    heat::clear_character(f.character);
    world.remove_ball(SYSTEM, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    combat::forget_ship(f.ship);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
