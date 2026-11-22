use super::*;
use crate::services::{effects::tests::fixture, tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation}};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_FITTING_FIRST, GM_ROLES, dogma::{ATTR_ARMOR_HP, ATTR_CAPACITOR_CAPACITY, ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT, ATTR_RECHARGE_RATE, ATTR_RESISTANCE_SHIFT_AMOUNT, EFFECT_ADAPTIVE_ARMOR_HARDENER}};
use crate::services::fit::reactive_armor::ARMOR_RESONANCE;

fn hit(system: &mut crate::space::System, source: i64, attacker: i64, victim: i64, weapon: Option<i32>, sequence: u64, raw: [f64; 4]) -> Option<crate::space::combat::ActorDamageOutcome> {
    let facts = system.prepare_combat_source(source, None, None, weapon, crate::lua::events::resources::FactOrigin::Natural, crate::space::targeting::next_generation(), sequence, 1, |_| true).unwrap();
    system.apply_damage(victim, raw, crate::services::combat::damage::DamageSource { attacker_id: Some(attacker), facts: Some(facts), ..Default::default() }, false).unwrap()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run this exact test in its own process"]
async fn cycle_end_adapts_primes_and_a_dropped_generation_cannot_clear_its_successor() {
    crate::tests::init_config();
    let db = fixture::cycle_observer(Db::connect(&crate::config::tests::test_database()).await.unwrap());
    let sde = crate::tests::shared_sde(&db).await;
    let effect = registry::resolve(&sde, EFFECT_ADAPTIVE_ARMOR_HARDENER).unwrap();
    let module_type = sde.types.iter().find(|ty| ty.effects().iter().any(|row| row.effect_id == EFFECT_ADAPTIVE_ARMOR_HARDENER) && ty.attribute(ATTR_RESISTANCE_SHIFT_AMOUNT).unwrap_or(0.0) > 0.0).unwrap().type_id;
    let data = sde.types.get(module_type).unwrap();
    let can_fit = |prefix: &str, padded: bool| (1..).map_while(|index| sde.dogma.attributes_by_name(&if padded { format!("{prefix}{index:02}") } else { format!("{prefix}{index}") }).next()).filter_map(|attribute| data.attribute(attribute.attribute_id)).map(|value| value as i32).filter(|value| *value > 0).collect::<Vec<i32>>();
    let (groups, types) = (can_fit("canFitShipGroup", true), can_fit("canFitShipType", false));
    let hull = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP) && (groups.is_empty() && types.is_empty() || groups.contains(&ty.group_id.unwrap_or(0)) || types.contains(&ty.type_id))).unwrap().type_id;
    let name = sde.dogma.effect(EFFECT_ADAPTIVE_ARMOR_HARDENER).unwrap().name.clone().unwrap();
    let duration = sde.dogma.effect(EFFECT_ADAPTIVE_ARMOR_HARDENER).and_then(|row| row.duration_attribute_id).expect("duration attribute");
    let (station, system) = sde.map.stations().find(|row| row.solar_system_id.is_some()).map(|row| (row.station_id, row.solar_system_id.unwrap())).unwrap();
    let ship = create_ship(&db, hull, ShipLocation::Docked { station }, &[ShipItemSpec { type_id: module_type, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, online: true, attributes: &[(duration, 1_500.0)] }]).await.unwrap();
    let attacker = create_ship(&db, hull, ShipLocation::Space { system }, &[]).await.unwrap();
    let module = ship.module();
    for &(item, attr, value) in &[(ship.ship, ATTR_CAPACITOR_CAPACITY, 1e9), (ship.ship, ATTR_RECHARGE_RATE, 1e6), (ship.ship, ATTR_CPU_OUTPUT, 1e9), (ship.ship, ATTR_POWER_OUTPUT, 1e9), (ship.ship, ATTR_ARMOR_HP, 1e6)] { crate::dao::items::dogma::set_override(&db, item, attr, value).await.unwrap(); }
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&ship.ship, &system]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2,solar_system_id=$3,station_id=NULL WHERE character_id=$1", &[&ship.character, &ship.ship, &system]).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), ship.character).await;
    client.login.role = GM_ROLES;
    client.session.apply(vec![("role", Value::Int(GM_ROLES)), ("corpid", Value::Int(1)), ("shipid", Value::Int(ship.ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx.clone(); client.world.register_pilot(ship.character, tx);
    client.world.set_online_session(ship.character, infinity_core::session::OnlineSession::from_session(&client.session));
    let required = crate::services::skills::requirements::requirements(&sde, module_type).collect::<Vec<_>>();
    crate::services::skills::gm::apply(&mut client, ship.character, crate::services::skills::gm::Change::Set(required)).await.unwrap();
    fixture::admit(&db, &client.world, system, fixture::ball(ship.ship, hull, ship.character, Some(ship.character), 0.0)).await;
    fixture::admit(&db, &client.world, system, fixture::ball(attacker.ship, hull, attacker.character, Some(attacker.character), 1_000.0)).await;
    let (world, ship_id, attacker_id) = (client.world.clone(), ship.ship, attacker.ship);
    let result = crate::tests::catch_scenario(async {
        let values = |key: Key| world.with(system, move |s| s.with_combat_fit(ship_id, move |fit, _, _| ARMOR_RESONANCE.map(|attr| fit.value(key, attr))).unwrap());
        let baseline = values(Key::Ship).await.unwrap();
        fixture::clear_cycles(module);
        fixture::dogma_call(&mut client, "Activate", &[Value::Int(module), Value::str(&name), Value::None, Value::Int(100)]).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async { while world.with(system, move |s| s.reactive_armor_readout(ship_id).is_none()).await.unwrap() { assert!(world.after(system, 200).await); } }).await.unwrap();
        let (_, seed, _) = world.with(system, move |s| s.reactive_armor_readout(ship_id)).await.unwrap().unwrap();
        let shift = world.with(system, move |s| s.with_combat_fit(ship_id, |fit, _, _| fit.value(Key::Module(module), ATTR_RESISTANCE_SHIFT_AMOUNT) / crate::services::fit::PERCENT_SCALE).unwrap()).await.unwrap();
        let old = world.with(system, move |_| state().active[&module].stop.clone()).await.unwrap();
        let effective = world.with(system, move |s| s.with_combat_fit(ship_id, |fit, _, _| fit.value(Key::Module(module), duration)).unwrap()).await.unwrap();
        assert_eq!(effective, 1_500.0); assert!(world.after(system, (effective / 2.0) as u64).await && fixture::completed_cycles(module) == 0);
        world.with(system, move |s| assert!(hit(s, attacker_id, attacker.character, ship_id, Some(module_type), 1, [1e5, 0.0, 0.0, 0.0]).is_some())).await.unwrap();
        assert_eq!(values(Key::Module(module)).await.unwrap(), seed);
        let step = |value: f64| (1.0 - value).min(shift);
        let expected = [seed[0] - step(seed[1]) - step(seed[2]) - step(seed[3]), seed[1] + step(seed[1]), seed[2] + step(seed[2]), seed[3] + step(seed[3])];
        tokio::time::timeout(std::time::Duration::from_millis(((effective * 2.0) as u64) + 5_000), async { while fixture::completed_cycles(module) < 1 { assert!(world.after(system, 200).await); } }).await.unwrap();
        let (_, module_now, ship_now) = world.with(system, move |s| s.reactive_armor_readout(ship_id)).await.unwrap().unwrap();
        for index in 0..4 { assert!((module_now[index] - expected[index]).abs() < 1e-9 && (ship_now[index] - baseline[index] * expected[index]).abs() < 1e-9); }
        for (item, published) in [(module, module_now), (ship_id, ship_now)] { for (attr, value) in ARMOR_RESONANCE.iter().zip(published) { assert_eq!(fixture::fitted_attr(&mut client, item, *attr).await, value); } }
        fixture::dogma_call(&mut client, "Deactivate", &[Value::Int(module), Value::str(&name)]).await.unwrap();
        let held = world.with(system, move |s| s.reactive_armor_readout(ship_id)).await.unwrap().map(|(_, _, ship)| ship); assert_eq!(held, Some(ship_now));
        tokio::time::timeout(std::time::Duration::from_millis(((effective * 3.0) as u64) + 5_000), async { while state().active.contains_key(&module) { assert!(world.after(system, 200).await); } }).await.unwrap();
        assert_eq!(fixture::completed_cycles(module), 2);
        assert_eq!(values(Key::Ship).await.unwrap(), baseline);
        while rx.try_recv().is_ok() {}
        fixture::dogma_call(&mut client, "Activate", &[Value::Int(module), Value::str(&name), Value::None, Value::Int(100)]).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async { while world.with(system, move |s| s.reactive_armor_readout(ship_id).is_none()).await.unwrap() { assert!(world.after(system, 200).await); } }).await.unwrap();
        let membership = world.with(system, move |s| s.combat_membership_generation(ship_id)).await.unwrap();
        drop(Cycle { cx: Ctx { db: db.clone(), world: world.clone(), notify: client.notify_tx.clone(), char_id: ship.character, ship_id, system_id: system }, modules: vec![Module { id: module, type_id: module_type, flag: FLAG_FITTING_FIRST }], effect: effect.clone(), presented_effect: effect.id, target: None, repeat: 1, stop: old, owner: ActivationOwner { master: module, generation: Arc::new(AtomicBool::new(true)), armed: false, combat_generations: vec![], pending_combat: None, world: std::sync::Weak::new(), system }, started: 0, charge_type: None, cycles: 1, duration_ms: 1.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false, source_membership: membership, target_type: None, inputs: parameters::CycleInputs::default() });
        world.with(system, |_| ()).await.unwrap(); assert!(world.with(system, move |s| s.reactive_armor_readout(ship_id).is_some()).await.unwrap());
    }).await;
    if let Some(active) = state().active.remove(&module) { active.stop.store(true, Ordering::Relaxed); }
    state().activation_generation.remove(&module); fixture::clear_cycles(module);
    cleanup_ship(&db, Some(&*world), ship).await.unwrap();
    cleanup_ship(&db, Some(&*world), attacker).await.unwrap();
    let mut fit = crate::services::fit::attributes::tests::module_fit(sde.clone(), hull, module_type, true);
    assert!(fit.set_module_active(2, true));
    let (shift, seed) = (fit.value(Key::Module(2), ATTR_RESISTANCE_SHIFT_AMOUNT) / crate::services::fit::PERCENT_SCALE, ARMOR_RESONANCE.map(|attr| fit.modifier_value(Key::Module(2), attr)));
    let bare = crate::services::fit::Fit::for_space_type(sde.clone(), hull, vec![]);
    let baseline = ARMOR_RESONANCE.map(|attr| bare.value(Key::Ship, attr));
    let (due, active_cap, expired_cap) = fixture::due_fit(fit, &sde, |f| f.value(Key::Ship, ATTR_CAPACITOR_CAPACITY));
    let (old_module, old_ship) = (ARMOR_RESONANCE.map(|attr| due.value(Key::Module(2), attr)), ARMOR_RESONANCE.map(|attr| due.value(Key::Ship, attr)));
    let (registry, (tx, mut rx)) = (std::sync::Arc::new(crate::space::Pilots::default()), tokio::sync::mpsc::unbounded_channel()); registry.register_pilot(2, tx);
    let mut system = crate::space::System::new(1, registry, 0); system.insert(fixture::ball(1, hull, 2, Some(2), 0.0)); system.insert(fixture::ball(3, hull, 3, Some(3), 1_000.0));
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&due, Key::Ship);
    system.install_condition(1, crate::services::combat::groups_health::Condition { now: [0.0, stats.max[1], stats.max[2]], stats, shield_at: crate::client::filetime_now() }, due);
    let attacker_stats = crate::services::combat::groups_health::ShipStats::from_fit(&bare, Key::Ship);
    system.install_condition(3, crate::services::combat::groups_health::Condition { now: attacker_stats.max, stats: attacker_stats, shield_at: crate::client::filetime_now() }, bare);
    let membership = system.combat_membership_generation(1).unwrap();
    let generation = Arc::new(AtomicBool::new(false));
    assert!(system.begin_reactive_armor(1, membership, 2, module_type, &generation, Some(seed)));
    assert!(hit(&mut system, 3, 3, 1, None, 1, [10.0, 0.0, 0.0, 0.0]).is_some());
    assert!(system.finish_reactive_armor_cycle(1, membership, 2, &generation, shift));
    let module_now = system.with_combat_fit(1, |fit, _, _| ARMOR_RESONANCE.map(|attr| fit.value(Key::Module(2), attr))).unwrap();
    let published: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(name, _)| name == "OnModuleAttributeChanges").collect();
    assert_eq!(published.len(), 1); let rows = published[0].1[0].items().unwrap();
    let row = |item: i64, attr: i32| rows.iter().find_map(|entry| { let fields = entry.items().unwrap(); (fields[2].as_int() == Some(item) && fields[3].as_int() == Some(i64::from(attr))).then(|| (fields[5].as_float().unwrap(), fields[6].as_float().unwrap())) }).unwrap_or_else(|| panic!("missing row {item}/{attr}"));
    let close = |actual: f64, wanted: f64| (actual - wanted).abs() < 1e-9;
    let (charged, was) = row(1, ATTR_CAPACITOR_CAPACITY); assert!(close(charged, expired_cap) && close(was, active_cap));
    for (index, &attr) in ARMOR_RESONANCE.iter().enumerate() {
        let (module_new, module_old) = row(2, attr);
        let (ship_new, ship_old) = row(1, attr);
        assert!(close(module_new, module_now[index]) && close(module_old, old_module[index]) && close(ship_new, baseline[index] * module_now[index]) && close(ship_old, old_ship[index]));
    }

    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
