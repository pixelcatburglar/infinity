//! Real shared-cycle fuel acceptance on a dedicated runtime.
use super::*;
use std::{sync::{Arc, Mutex, atomic::AtomicBool}, time::Duration};
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_FITTING_FIRST, dogma::{ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY, ATTR_CAPACITOR_NEED, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_RECHARGE_RATE, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_RECHARGE_RATE, ATTR_SPEED_FACTOR, ATTR_STRUCTURE_HP, EFFECT_MICROWARPDRIVE}}};
use crate::{dao::items::dogma, db::Db, sde::Sde, services::{combat::{self, ActiveModule, groups_health::{capacitor_snapshot, Condition, ShipStats}}, effects::{EffectKind, lifecycle::{ActivationOwner, Cycle, cycle_start}, registry::{self, EffectAttribute, EffectFlag}}}, space::{Ball, WarpState, World, MODE_STOP}};

#[derive(Clone, Copy)]
enum Scenario { OutOfCap, ShortFuel, Propulsion, LocalRepair }

#[derive(Clone, Copy, Default)]
struct Ids { account: i64, owner: i64, ship: i64, module: i64, stack: i64 }

fn run(scenario: Scenario) {
    crate::tests::init_config();
    let ids = Arc::new(Mutex::new(Ids::default()));
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let shared = ids.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(async {
        let mut task = tokio::spawn(async move { scenario_run(scenario, shared).await });
        match tokio::time::timeout(Duration::from_secs(90), &mut task).await {
            Ok(Ok(())) => {},
            Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Ok(Err(error)) => panic!("fuel lifecycle task cancelled: {error}"),
            Err(_) => { task.abort(); let _ = task.await; panic!("fuel lifecycle scenario timed out"); }
        }
    })));
    drop(runtime);
    let ids = *ids.lock().unwrap();
    if ids.module != 0 {
        let mut state = combat::state();
        state.active.remove(&ids.module);
        state.activation_generation.remove(&ids.module);
        drop(state);
        crate::services::effects::heat::clear_modules(ids.ship, &[ids.module]);
    }
    if ids.ship != 0 { combat::forget_ship(ids.ship); }
    let cleanup_runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup_runtime.block_on(async {
        let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[ids.ship, ids.module, ids.stack][..]]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id = ANY($1) AND owner_id=$2", &[&&[ids.stack, ids.module, ids.ship][..], &ids.owner]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&ids.owner, &ids.account]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&ids.account]).await.unwrap();
    })));
    if let Err(error) = cleanup {
        eprintln!("fuel lifecycle fixture cleanup failed: {error:?}");
        if let Err(original) = result { std::panic::resume_unwind(original); }
        std::panic::resume_unwind(error);
    }
    if let Err(error) = result { std::panic::resume_unwind(error); }
}

async fn scenario_run(scenario: Scenario, recorded: Arc<Mutex<Ids>>) {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let positive = |value: Option<f64>| value.is_some_and(|v| v.is_finite() && v > 0.0);
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && positive(t.fit_attribute(ATTR_CAPACITOR_CAPACITY)) && positive(t.fit_attribute(ATTR_MASS))
        && positive(t.fit_attribute(ATTR_MAX_VELOCITY)) && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE].iter().all(|&attr| positive(t.fit_attribute(attr)))).expect("ship with capacitor, health, mass and speed in SDE").type_id;
    let speed_boost = sde.dogma.attributes_by_name("speedBoostFactor").next().expect("speedBoostFactor in SDE").attribute_id;
    let (module_type, effect) = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_MODULE)
        && (matches!(scenario, Scenario::LocalRepair) || positive(t.fit_attribute(ATTR_SPEED_FACTOR)) && positive(t.fit_attribute(speed_boost)))).find_map(|t| {
        t.effects().iter().filter_map(|e| registry::resolve(&sde, e.effect_id)).find(|effect| {
            let kind_matches = if matches!(scenario, Scenario::LocalRepair) {
                matches!(effect.kind, EffectKind::LocalRepair { layer: 1, .. })
            } else {
                effect.kind == EffectKind::Propulsion && effect.id == i64::from(EFFECT_MICROWARPDRIVE)
            };
            kind_matches && effect.manually_activatable() && !effect.has(EffectFlag::DisallowAutoRepeat)
                && effect.attribute(EffectAttribute::Duration).is_some()
        }).map(|effect| (t.type_id, effect))
    }).expect("repeatable supported module effect in SDE");
    let fuel = sde.types.iter().find(|t| t.group_id == Some(GROUP_ICE_PRODUCT)).expect("ice product in SDE").type_id;
    let row = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS account, nextval('game.characters_character_id_seq') AS owner, nextval('game.items_item_id_seq') AS ship, nextval('game.items_item_id_seq') AS module, nextval('game.items_item_id_seq') AS stack", &[]).await.unwrap();
    let ids = Ids { account: row.get("account"), owner: row.get("owner"), ship: row.get("ship"), module: row.get("module"), stack: row.get("stack") };
    *recorded.lock().unwrap() = ids;
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,'fuel-cycle-' || $1::bigint::text,$2,0)", &[&ids.account, &&b"fixture"[..]]).await.unwrap();
    db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,'fuel-cycle-' || $1::bigint::text,1,1,1,1)", &[&ids.owner, &ids.account]).await.unwrap();
    let insert = "INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,online,x,y,z) VALUES ($1,$2,$3,$4,$5,$6,$7,true,0,0,0)";
    db.execute(insert, &[&ids.ship,&ship_type,&ids.owner,&1_i64,&0_i32,&1_i64,&true]).await.unwrap();
    db.execute(insert, &[&ids.module,&module_type,&ids.owner,&ids.ship,&FLAG_FITTING_FIRST,&1_i64,&true]).await.unwrap();
    let quantity = if matches!(scenario, Scenario::ShortFuel) { 1_i64 } else { 6_i64 };
    db.execute(insert, &[&ids.stack,&fuel,&ids.owner,&ids.ship,&FLAG_CARGO,&quantity,&false]).await.unwrap();
    for (item, attr, value) in [(ids.module, ATTR_CONSUMPTION_TYPE, f64::from(fuel)), (ids.module, ATTR_CONSUMPTION_QUANTITY, 2.0), (ids.module, effect.attribute(EffectAttribute::Duration).unwrap(), 2000.0), (ids.module, effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED), if matches!(scenario, Scenario::OutOfCap) { 2000.0 } else { 10.0 }), (ids.ship, ATTR_CAPACITOR_CAPACITY, 1000.0), (ids.ship, ATTR_RECHARGE_RATE, 1e15)] {
        dogma::set_override(&db, item, attr, value).await.unwrap();
    }
    if let EffectKind::LocalRepair { amount_attr, .. } = effect.kind {
        dogma::set_override(&db, ids.module, amount_attr, 10.0).await.unwrap();
        dogma::set_override(&db, ids.ship, ATTR_ARMOR_HP, 1000.0).await.unwrap();
    }
    let world = World::new(db.clone(), sde.clone(), 1);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(ids.owner, sender.clone());
    let ball = Ball { id: ids.ship, type_id: ship_type, owner_id: ids.owner, corp_id: 1, alliance_id: -1, char_id: Some(ids.owner), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false, warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() };
    world.add_ship(1, ball).await.unwrap();
    let (destiny_tx, mut destiny_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(1, ids.owner, ids.ship, destiny_tx).await.unwrap();
    let fit = Fit::load_in_space(&db, &world, 1, ids.owner, ids.ship).await.unwrap();
    assert_eq!(fit.value(Key::Module(ids.module), ATTR_CONSUMPTION_TYPE), f64::from(fuel));
    assert_eq!(fit.value(Key::Module(ids.module), ATTR_CONSUMPTION_QUANTITY), 2.0);
    let baseline_speed = fit.value(Key::Ship, ATTR_MAX_VELOCITY);
    assert!(baseline_speed.is_finite() && baseline_speed > 0.0);
    let repair_before = if matches!(scenario, Scenario::LocalRepair) {
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        let before = stats.max[1] * 0.5;
        assert!(before + 20.0 < stats.max[1]);
        let condition = Condition { now: stats.max.map(|value| value * 0.5), stats, shield_at: crate::client::filetime_now() + 60 * infinity_core::consts::FILETIME_SECOND };
        world.with(1, move |s| s.install_condition(ids.ship, condition, fit)).await.unwrap();
        Some(before)
    } else { None };
    let effect_id = effect.id;
    let cx = Ctx { db: db.clone(), world: world.clone(), notify: sender, char_id: ids.owner, ship_id: ids.ship, system_id: 1 };
    let generation = Arc::new(AtomicBool::new(false));
    { let mut state = combat::state(); state.active.insert(ids.module, ActiveModule { char_id: ids.owner, stop: generation.clone(), forced_inactive: false }); state.activation_generation.insert(ids.module, generation.clone()); }
    let modules = vec![Module { id: ids.module, type_id: module_type, flag: FLAG_FITTING_FIRST }];
    let combat_generations = modules.iter().map(|module| crate::services::effects::lifecycle::NativeCombatGeneration {
        module_id: module.id, generation: crate::space::targeting::next_generation(), sequence: 0,
    }).collect();
    let owner = ActivationOwner { master: ids.module, generation: generation.clone(), armed: true,
        combat_generations, pending_combat: None, world: Arc::downgrade(&world), system: 1 };
    let cycle = Cycle { cx, modules, effect, target: None, repeat: 2, stop: generation, owner, started: 1, charge_type: None, cycles: 0, duration_ms: 500.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
    world.with(1, move |s| { let now = s.now_ms(); cycle_start(s, cycle, now); }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let retired = { let state = combat::state(); !state.active.contains_key(&ids.module) && !state.activation_generation.contains_key(&ids.module) };
            if retired { break; }
            assert!(world.after(1, 100).await);
        }
    }).await.expect("cycle and deferred propulsion retirement");
    let mut events = Vec::new();
    if matches!(scenario, Scenario::Propulsion) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let speed = world.with(1, move |s| s.ball(ids.ship).map(|ball| f64::from(ball.max_vel))).await.unwrap().unwrap();
                events.extend(std::iter::from_fn(|| destiny_rx.try_recv().ok()).flatten());
                let mut boosted = false;
                let restored = events.iter().any(|event| {
                    let value = match event {
                        Value::Tuple(stamped) => match stamped.get(1) {
                            Some(Value::Tuple(call)) if call.first() == Some(&Value::str("SetMaxSpeed")) => match call.get(1) {
                                Some(Value::Tuple(args)) if args.first() == Some(&Value::Int(ids.ship)) => match args.get(1) { Some(Value::Float(value)) => Some(*value), _ => None },
                                _ => None,
                            },
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(value) = value {
                        if value > baseline_speed { boosted = true; }
                        else if boosted && (value - baseline_speed).abs() <= baseline_speed * 1e-10 { return true; }
                    }
                    false
                });
                // Ball::set_max_speed stores the fitted speed as f32.
                if restored && speed == f64::from(baseline_speed as f32) { break; }
                assert!(world.after(1, 100).await);
            }
        }).await.expect("propulsion baseline applied to actor");
    }
    if let Some(before) = repair_before {
        let repaired = world.with(1, move |s| s.combat_condition_snapshot(ids.ship).unwrap().condition.now[1]).await.unwrap();
        assert!((repaired - (before + 20.0)).abs() < 1e-6);
    }
    let stack = load_items(&db, &sde, ItemQuery::Id(ids.stack)).await.unwrap().remove(0);
    let expected = if matches!(scenario, Scenario::OutOfCap | Scenario::ShortFuel) { quantity } else { 2 };
    assert_eq!(stack.quantity, expected);
    let notices: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
    assert_eq!(notices.iter().filter(|(name, args)| name == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(ids.module)) && args.get(1) == Some(&Value::Int(effect_id)) && args.get(3) == Some(&Value::Int(0))).count(), 1);
    let item_notices: Vec<_> = notices.iter().filter(|(name, _)| name == "OnItemChange").collect();
    events.extend(std::iter::from_fn(|| destiny_rx.try_recv().ok()).flatten());
    let calls: Vec<_> = events.iter().filter_map(|event| match event {
        Value::Tuple(stamped) => match stamped.get(1) { Some(Value::Tuple(call)) => Some(call), _ => None },
        _ => None,
    }).collect();
    let speeds: Vec<_> = calls.iter().filter_map(|call| {
        if call.first() != Some(&Value::str("SetMaxSpeed")) { return None; }
        match call.get(1) { Some(Value::Tuple(args)) if args.first() == Some(&Value::Int(ids.ship)) => match args.get(1) { Some(Value::Float(speed)) => Some(*speed), _ => None }, _ => None }
    }).collect();
    if matches!(scenario, Scenario::OutOfCap | Scenario::ShortFuel) {
        assert!(item_notices.is_empty());
        let expected_message = if matches!(scenario, Scenario::OutOfCap) { vec![Value::str("NotEnoughCharge"), Value::Dict(vec![])] } else { vec![Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), Value::Dict(vec![(Value::str("notify"), Value::unicode(infinity_core::messages::effects::INSUFFICIENT_FUEL))])] };
        assert!(notices.iter().any(|(name, args)| name == "OnEffectMessage" && *args == expected_message));
        if matches!(scenario, Scenario::OutOfCap) {
            let fuel_message = vec![Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), Value::Dict(vec![(Value::str("notify"), Value::unicode(infinity_core::messages::effects::INSUFFICIENT_FUEL))])];
            assert!(!notices.iter().any(|(name, args)| name == "OnEffectMessage" && *args == fuel_message));
        }
        assert!(!calls.iter().any(|call| call.first() == Some(&Value::str("OnSpecialFX"))));
        assert!(speeds.iter().all(|speed| *speed <= baseline_speed * (1.0 + 1e-10)));
        if let Some(racks) = crate::services::effects::heat::ship_state(&world, 1, ids.ship).await {
            assert!(racks.iter().all(|rack| rack.incoming == 0.0 && rack.heat == 0.0));
        }
        assert!(!crate::services::effects::heat::overloaded_modules().contains(&ids.module));
        if matches!(scenario, Scenario::ShortFuel) {
            let cap = capacitor_snapshot(&db, &world, 1, ids.ship).await.unwrap().unwrap();
            assert_eq!(notices.iter().filter(|(name, _)| name == "OnModuleAttributeChanges").count(), 2);
            assert!((cap.level - cap.capacity).abs() < 1e-6);
        }
    } else {
        if matches!(scenario, Scenario::Propulsion) {
            assert!(speeds.iter().any(|speed| *speed > baseline_speed));
            assert!(speeds.iter().any(|speed| (*speed - baseline_speed).abs() <= baseline_speed * 1e-10));
        }
        assert_eq!(item_notices.len(), 2);
        for (notice, old) in item_notices.iter().zip([6_i64, 4]) {
            let mut remaining = stack.clone();
            remaining.quantity = old - 2;
            assert_eq!(notice.1, vec![remaining.row(), Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(old))]), Value::None]);
        }
    }
}

#[test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
fn shared_cycle_out_of_cap_does_not_consume_fuel() { run(Scenario::OutOfCap); }
#[test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
fn shared_cycle_short_fuel_refunds_capacitor() { run(Scenario::ShortFuel); }
#[test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
fn propulsion_two_scheduled_cycles_consume_once_each() { run(Scenario::Propulsion); }
#[test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
fn local_repair_two_scheduled_cycles_consume_once_each() { run(Scenario::LocalRepair); }
