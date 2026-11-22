//! Database-backed cloak, micro jump and compression actor regressions.
use super::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use infinity_core::{consts::{dogma::ATTR_BASE_WARP_SPEED, dogma::ATTR_GATE_SCRAMBLE_STATUS, dogma::ATTR_HI_SLOTS, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_WARP_SCRAMBLE_STATUS, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_SCAN_RESOLUTION, dogma::ATTR_DISALLOW_IN_EMPIRE_SPACE, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_CLOAKING, dogma::EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT, dogma::EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT, dogma::EFFECT_MICRO_JUMP_DRIVE, dogma::EFFECT_MICROWARPDRIVE, dogma::EFFECT_INDUSTRIAL_ITEM_COMPRESSION, dogma::EFFECT_WARP_DISRUPT_SPHERE, dogma::ATTR_CPU, dogma::ATTR_CPU_OUTPUT, dogma::ATTR_POWER, dogma::ATTR_POWER_OUTPUT, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST}};
use crate::{dao::items::dogma, services::{combat::{self, tests::remote_repair_db::set_locked_target, ActiveModule, Ctx, targeting_charges}, effects::{cloak, compression, mjd, EffectKind, JammingType, lifecycle::{self, ActivationOwner, Cycle, Module}, parameters::MemberPacket, registry}, fit::{Fit, Key}, identity::Identity, CallError}, space::World};
use marshal::Value;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

async fn scenario(effect_id: i32, kind: u8) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (hull, module_type) = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP)).find_map(|h|
        sde.types.iter().find(|m| m.category_id == Some(CATEGORY_MODULE) && m.effects().iter().any(|e| e.effect_id == effect_id)
            && m.attribute(ATTR_CPU).zip(h.fit_attribute(ATTR_CPU_OUTPUT)).is_some_and(|(need, out)| out >= need)
            && m.attribute(ATTR_POWER).zip(h.fit_attribute(ATTR_POWER_OUTPUT)).is_some_and(|(need, out)| out >= need))
            .map(|m| (h.type_id, m.type_id))).expect("legal effect module and hull pair");
    let effect = registry::resolve(&sde, effect_id).expect("effect metadata");
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: module_type, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        // Overrides are instance dogma values, not type defaults.
        for (item, attr, value) in match kind {
            0 => vec![(f.ship, 1163, 1.0), (f.ship, 2454, 0.0), (module_id, 669, 4000.0), (module_id, 560, 1000.0), (f.ship, ATTR_MAX_TARGET_RANGE, 100000.0), (f.ship, ATTR_SCAN_RESOLUTION, 1000000.0)],
            1 => vec![(module_id, 2066, 100_000.0), (f.ship, ATTR_WARP_SCRAMBLE_STATUS, 0.0)],
            _ => vec![(module_id, 3255, 7.0), (module_id, ATTR_MAX_RANGE, 20000.0), (module_id, infinity_core::consts::dogma::ATTR_ACTIVATION_REQUIRES_ACTIVE_INDUSTRIAL_CORE, 0.0)],
        } { dogma::set_override(&test_db, item, attr, value).await.unwrap(); }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }], presented_effect: effect.id, effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module_id, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
            source_membership: None, target_type: None, inputs: crate::services::effects::parameters::CycleInputs::default() };
        cycle = fixture::capture_cycle(cycle).await;
        let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("load fit");
        match kind {
            0 => {
                test_world.add_ship(1, fixture::ball(-f.ship, hull, f.character, None, 5000.0)).await.unwrap();
                targeting_charges::add_target_ctx(&cx, -f.ship, true).await.ok().expect("lock target");
                assert!(test_world.after(1, 1200).await);
                assert!(targeting_charges::targets_of(f.character).contains(&-f.ship));
                cloak::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.expect("cloak valid");
                let hp = combat::load_docked_condition(&test_db, &test_world, f.character, f.ship).await.ok().unwrap();
                cloak::start(&cx, &mut cycle).await.ok().unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    while let Some((name, args)) = rx.recv().await {
                        if name == "OnTarget" && args.first().and_then(Value::as_bytes) == Some(b"lost".as_slice())
                            && args.get(1).and_then(Value::as_int) == Some(-f.ship) { return; }
                    }
                    panic!("target notification channel closed");
                }).await.expect("cloak target loss notification");
                assert!(!targeting_charges::targets_of(f.character).contains(&-f.ship));
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().cloak, 1);
                assert!(cloak::before_lock(&cx).await.is_err());
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().cloak, 0);
                assert!(cloak::before_lock(&cx).await.is_ok());
                assert!(cloak::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.is_err());
                assert_eq!(combat::load_docked_condition(&test_db, &test_world, f.character, f.ship).await.ok().unwrap(), hp);
                assert!(test_world.after(1, 3100).await);
                assert!(cloak::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.is_ok());
                generation.store(false, Ordering::Relaxed);
                cloak::start(&cx, &mut cycle).await.ok().unwrap();
                assert!(test_world.after(1, 1200).await);
                cloak::before_activate(&cx, module_id, EffectKind::Turret).await.ok().unwrap();
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().cloak, 0);
                let stop_cx = cx.clone();
                test_world.with(1, move |s| cloak::stop(s, &stop_cx, &cycle)).await.unwrap();
            }
            1 => {
                let (mut client, _peer) = crate::services::tests::client(test_db.clone(), f.character).await;
                client.world = test_world.clone();
                client.session.apply(vec![("shipid", Value::Int(f.ship))]);
                client.session.apply(vec![("solarsystemid", Value::Int(1))]);
                client.notify_tx = cx.notify.clone();
                let required = crate::services::skills::requirements::requirements(&sde, module_type).collect::<Vec<_>>();
                crate::services::skills::gm::apply(&mut client, f.character, crate::services::skills::gm::Change::Set(required)).await.unwrap();
                fixture::gm_value(&mut client, f.ship, ATTR_WARP_SCRAMBLE_STATUS, 1.0).await;
                assert!(dogma::online(&test_db, f.ship).await.unwrap().iter().any(|m| m.item_id == module_id), "fixture pilot must meet the module's prerequisites after first GM write (module {})", module_id);
                let blocked = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().unwrap();
                assert!(mjd::validate(&cx, &blocked, &cycle.modules[0], &cycle.effect, None).await.is_err());
                assert!(matches!(mjd::end(&cx, &mut cycle).await.ok().unwrap(), crate::services::effects::lifecycle::CycleOutcome::Stop(_)));
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().pos, [0.0; 3]);
                fixture::gm_value(&mut client, f.ship, ATTR_WARP_SCRAMBLE_STATUS, 0.0).await;
                let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("reload fit");
                mjd::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.ok().unwrap();
                assert_eq!(mjd::start(&cx, &mut cycle).await.ok().unwrap(), crate::services::effects::lifecycle::CycleOutcome::Continue);
                assert!(matches!(mjd::end(&cx, &mut cycle).await.ok().unwrap(), crate::services::effects::lifecycle::CycleOutcome::Ended { outcome: crate::lua::events::ModuleCycleOutcome::Completed, .. }));
                let next_generation = Arc::new(AtomicBool::new(false));
                let mut next = Cycle { cx: cx.clone(), modules: cycle.modules.clone(), presented_effect: cycle.presented_effect, effect: cycle.effect.clone(),
                    target: None, repeat: 1, stop: next_generation.clone(),
                    owner: ActivationOwner { master: module_id, generation: next_generation.clone(), armed: true,
                        combat_generations: vec![], pending_combat: None, world: std::sync::Weak::new(), system: 1 },
                    started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0,
                    other: None, repair_amount: 0.0, illegal: false, source_membership: None, target_type: None,
                    inputs: crate::services::effects::parameters::CycleInputs::default() };
                // Normal completion retires the activation before the destiny action executes.
                drop(cycle);
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().pos, [0.0, 0.0, -100_000.0]);
                combat::state().active.insert(module_id, ActiveModule { char_id: f.character, stop: next_generation.clone(), forced_inactive: false });
                combat::state().activation_generation.insert(module_id, next_generation);
                next = fixture::capture_cycle(next).await;
                assert_eq!(mjd::start(&cx, &mut next).await.unwrap(), crate::services::effects::lifecycle::CycleOutcome::Continue);
                mjd::end(&cx, &mut next).await.unwrap();
                drop(next);
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().pos, [0.0, 0.0, -200_000.0]);
            }
            _ => {
                compression::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.ok().unwrap();
                compression::start(&cx, &mut cycle).await.ok().unwrap();
                let marker = |b: &crate::space::Ball| b.slim.iter().find(|(key, _)| *key == "compression_facility_typelists").map(|(_, value)| value.clone());
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20000.0))])));
                let other = Arc::new(AtomicBool::new(false));
                let other_generation = other.clone();
                test_world.with(1, move |s| s.set_compressor(f.ship, -module_id, other_generation, 8, 30000.0)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20000.0)), (Value::Int(8), Value::Float(30000.0))])));
                let stale = Arc::new(AtomicBool::new(false));
                let stale_cycle = Cycle { cx: cx.clone(), modules: cycle.modules.clone(), presented_effect: cycle.presented_effect, effect: cycle.effect.clone(), target: None, repeat: 1,
                    stop: stale.clone(), owner: ActivationOwner { master: module_id, generation: stale, armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 }, started: 0,
                    charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
                    source_membership: None, target_type: None, inputs: crate::services::effects::parameters::CycleInputs::default() };
                let stale_cx = cx.clone();
                test_world.with(1, move |s| compression::stop(s, &stale_cx, &stale_cycle)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20000.0)), (Value::Int(8), Value::Float(30000.0))])));
                let stop_cx = cx.clone();
                test_world.with(1, move |s| compression::stop(s, &stop_cx, &cycle)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(8), Value::Float(30000.0))])));
                test_world.with(1, move |s| s.remove_compressor(f.ship, -module_id, &other)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), None);
            }
        }
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    world.remove_ball(1, -f.ship).await;
    world.remove_ball(1, f.ship).await;
    combat::forget_ship(f.ship);
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn cloak_fitted_delays_and_visibility() { scenario(EFFECT_CLOAKING, 0).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn micro_jump_survives_normal_retirement() { scenario(EFFECT_MICRO_JUMP_DRIVE, 1).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn compression_advertises_fitted_mapping() { scenario(EFFECT_INDUSTRIAL_ITEM_COMPRESSION, 2).await; }

async fn focused(script: i32, actual: i32, optimal: f64, jamming: JammingType) {
    crate::tests::init_config();
    let db = fixture::cycle_observer(crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap());
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    // The fixture lives in a real low-security system: an unscripted sphere is
    // refused by the module's own disallowInEmpireSpace while the loaded
    // script's projected admission still works.
    let lowsec = sde.map.systems().find(|s| s.security.is_some_and(|v| v > 0.0
        && v < crate::config::game().crimewatch.highsec_threshold)).expect("SDE low-security system").solar_system_id;
    // The approved scripted module is the WDFG I; its hull must legally fit two
    // of them without a hull-side scrambler range bonus (moduleGroup889Enhance).
    let wdfg = sde.types.get(28654).expect("Warp Disruption Field Generator I").type_id;
    let module = sde.types.get(wdfg).unwrap();
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_HI_SLOTS).is_some_and(|slots| slots >= 2.0)
        && module.attribute(ATTR_CPU).zip(t.fit_attribute(ATTR_CPU_OUTPUT)).is_some_and(|(need, out)| out >= need * 2.0)
        && module.attribute(ATTR_POWER).zip(t.fit_attribute(ATTR_POWER_OUTPUT)).is_some_and(|(need, out)| out >= need * 2.0)
        && !t.effects().iter().any(|e| e.effect_id == 11729)).expect("hull legally fitting two generators without a range bonus").type_id;
    let (target_hull, mwd_type) = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.type_id != hull
        && Fit::for_space_type(sde.clone(), t.type_id, vec![]).value(Key::Ship, ATTR_GATE_SCRAMBLE_STATUS) == -1000.0
        && Fit::for_space_type(sde.clone(), t.type_id, vec![]).value(Key::Ship, ATTR_BASE_WARP_SPEED) > 0.0)
        .find_map(|h| sde.types.iter().find(|m| m.category_id == Some(CATEGORY_MODULE) && m.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
            && m.attribute(ATTR_CPU).zip(h.fit_attribute(ATTR_CPU_OUTPUT)).is_some_and(|(need, out)| out >= need)
            && m.attribute(ATTR_POWER).zip(h.fit_attribute(ATTR_POWER_OUTPUT)).is_some_and(|(need, out)| out >= need))
            .map(|m| (h.type_id, m.type_id))).expect("target hull legally fitting a microwarpdrive");
    let source = create_ship(&db, hull, ShipLocation::Space { system: lowsec }, &[
        ShipItemSpec { type_id: wdfg, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: wdfg, flag_id: FLAG_HI_SLOT_FIRST + 1, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let target = create_ship(&db, target_hull, ShipLocation::Space { system: lowsec }, &[
        ShipItemSpec { type_id: mwd_type, flag_id: FLAG_MED_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (scripted, bare, mwd_item) = (source.items[0], source.items[1], target.items[0]);
    crate::dao::items::combat::apply_loads(&db, source.ship, &[(scripted, FLAG_HI_SLOT_FIRST, script, 1, 1)]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(source.character, tx.clone());
    fixture::admit(&db, &world, lowsec, fixture::ball(source.ship, hull, source.character, Some(source.character), 0.0)).await;
    fixture::admit(&db, &world, lowsec, fixture::ball(target.ship, target_hull, target.character, Some(target.character), 5_000.0)).await;
    let (test_world, test_db) = (world.clone(), db.clone());
    let (src_char, src_ship, src_acct, tgt_char, tgt_ship, tgt_acct) =
        (source.character, source.ship, source.account, target.character, target.ship, target.account);
    let result = crate::tests::catch_scenario(async move {
        // Every OnGodmaShipEffect for `module` is collected and must present the
        // requested effect both in args and inside the environment tuple.
        let godma = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, module: i64| {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter(|(name, args)| name == "OnGodmaShipEffect" && args.first() == Some(&Value::Int(module)))
                .map(|(_, args)| {
                    assert_eq!(args.get(1), Some(&Value::Int(i64::from(EFFECT_WARP_DISRUPT_SPHERE))));
                    assert_eq!(args.get(5).and_then(|v| v.items()).and_then(|items| items.get(6)), Some(&Value::Int(i64::from(EFFECT_WARP_DISRUPT_SPHERE))));
                    args.get(3).cloned()
                }).collect::<Vec<_>>()
        };
        let identity = Identity { char_id: src_char, ship_id: Some(src_ship), corp_id: 1, station_id: None, system_id: Some(lowsec) };
        assert_eq!(test_world.with(lowsec, move |s| s.loaded_effective(src_ship, scripted)).await.flatten().map(|(_, ty, qty, _)| (ty, qty)), Some((script, 1)));
        assert!(test_world.with(lowsec, move |s| s.loaded_effective(src_ship, bare)).await.flatten().is_none());
        let Err(CallError::User { msg, dict }) = lifecycle::activate(&identity, &test_db, &test_world, &sde, Some(lowsec), tx.clone(), src_acct, bare, "warpDisruptSphere", Some(tgt_ship), 0, |_| Ok(())).await
            else { panic!("unscripted sphere must refuse in empire space") };
        assert_eq!(msg, infinity_core::messages::MSG_CANT_IN_EMPIRE_SPACE);
        assert!(dict.is_empty());
        assert!(test_world.with(lowsec, move |s| s.projections_on(tgt_ship).is_empty()).await.unwrap());
        assert!(godma(&mut rx, bare).is_empty());
        set_locked_target(src_char, tgt_ship, true);
        let target_identity = Identity { char_id: tgt_char, ship_id: Some(tgt_ship), corp_id: 1, station_id: None, system_id: Some(lowsec) };
        let mwd_name = sde.dogma.effect(EFFECT_MICROWARPDRIVE).unwrap().name.clone().unwrap();
        let base_speed = test_world.with(lowsec, move |s| s.with_combat_fit(tgt_ship, |fit, _, _| fit.value(Key::Ship, ATTR_MAX_VELOCITY))).await.flatten().unwrap();
        lifecycle::activate(&target_identity, &test_db, &test_world, &sde, Some(lowsec), tx.clone(), tgt_acct, mwd_item, &mwd_name, None, 1_000, |_| Ok(())).await.unwrap();
        assert!(test_world.after(lowsec, 300).await);
        assert!(combat::state().active.contains_key(&mwd_item));
        let boosted = test_world.with(lowsec, move |s| s.with_combat_fit(tgt_ship, |fit, _, _| fit.value(Key::Ship, ATTR_MAX_VELOCITY))).await.flatten().unwrap();
        assert!(boosted > base_speed, "running microwarpdrive did not raise maxVelocity");
        lifecycle::activate(&identity, &test_db, &test_world, &sde, Some(lowsec), tx.clone(), src_acct, scripted, "warpDisruptSphere", Some(tgt_ship), 1_000, |_| Ok(())).await.unwrap();
        assert!(test_world.after(lowsec, 1_000).await);
        // Scripted admission works because the loaded script zeroes the
        // module's effective disallowInEmpireSpace, and it raises no field.
        let disallow = test_world.with(lowsec, move |s| s.with_combat_fit(src_ship,
            |fit, _, _| fit.value(Key::Module(scripted), ATTR_DISALLOW_IN_EMPIRE_SPACE))).await.flatten().unwrap();
        assert_eq!(disallow, 0.0);
        assert!(!test_world.with(lowsec, move |s| s.interdicted(src_ship)).await.unwrap());
        assert!(!test_world.with(lowsec, move |s| s.interdicted(tgt_ship)).await.unwrap());
        let stop = combat::state().active.get(&scripted).expect("scripted activation").stop.clone();
        let mut cycle = Cycle { cx: Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx.clone(), char_id: src_char, ship_id: src_ship, system_id: lowsec },
            modules: vec![Module { id: scripted, type_id: wdfg, flag: FLAG_HI_SLOT_FIRST }],
            presented_effect: i64::from(EFFECT_WARP_DISRUPT_SPHERE), effect: registry::resolve(&sde, actual).unwrap(),
            target: Some(tgt_ship), repeat: 1, stop: stop.clone(),
            owner: ActivationOwner { master: scripted, generation: stop, armed: false, combat_generations: vec![], pending_combat: None, world: std::sync::Weak::new(), system: lowsec },
            started: 0, charge_type: None, cycles: 0, duration_ms: 6_000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
            source_membership: None, target_type: None, inputs: Default::default() };
        assert!(lifecycle::has_effect(sde.types.effects(wdfg), EFFECT_WARP_DISRUPT_SPHERE) && !lifecycle::has_effect(sde.types.effects(wdfg), actual));
        cycle = fixture::capture_cycle(cycle).await;
        let frame = cycle.inputs.frame.as_ref().unwrap();
        let expected = 6_000.0;
        assert!((frame.duration_ms - expected).abs() <= f64::EPSILON * expected,
            "duration_ms {actual} is not within one ulp of {expected}", actual = frame.duration_ms);
        assert_eq!(frame.discharge, 60.0);
        assert_eq!(frame.master_charge_type, Some(script));
        let MemberPacket::Projected(inputs) = &cycle.inputs.members[0].packet else { panic!("projected member packet") };
        assert_eq!(inputs.ranges.optimal, optimal);
        assert!(lifecycle::cycle_subject_valid(&cycle).await);
        // The script's discharge attribute is honoured when explicitly present, zero included.
        let discharge_attr = cycle.effect.attribute(registry::EffectAttribute::Discharge).unwrap();
        for (patch, expected) in [(Some(0.0), 0.0), (Some(7.0), 7.0), (None, 60.0)] {
            let generation = test_world.with(lowsec, move |s| s.persistent_generation(src_ship)).await.flatten().unwrap();
            assert!(test_world.with(lowsec, move |s| s.publish_fitting(src_ship, src_char, generation,
                crate::services::fit::attributes::CommittedFitting::overridden_attributes(vec![(Key::Module(scripted), discharge_attr, patch)]), vec![])).await.unwrap());
            cycle.source_membership = None;
            cycle = fixture::capture_cycle(cycle).await;
            assert_eq!(cycle.inputs.frame.as_ref().unwrap().discharge, expected, "patched discharge attribute");
        }
        let projections = test_world.with(lowsec, move |s| s.projections_on(tgt_ship)).await.unwrap();
        assert_eq!(projections.len(), 1);
        assert_eq!((projections[0].effect_id, projections[0].kind), (actual, EffectKind::Projected { jamming }));
        assert_eq!((projections[0].charge_type, projections[0].charge_values.is_empty()), (None, true));
        let fit = Fit::load_in_space(&test_db, &test_world, lowsec, tgt_char, tgt_ship).await.unwrap();
        assert_eq!(fit.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS), 100.0);
        assert_eq!(fit.value(Key::Ship, ATTR_GATE_SCRAMBLE_STATUS), -999.0);
        let destination = crate::space::system::warp::WarpDestination::Exact([1e9, 0.0, 0.0]);
        assert!(matches!(crate::services::ship::warp_pilot(&test_db, &test_world, lowsec, tgt_char, tgt_ship, destination, None, None).await,
            Err(CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY
                && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::effects::WARP_SCRAMBLED))]));
        assert_eq!(combat::state().active.get(&mwd_item).map(|active| active.forced_inactive), Some(matches!(jamming, JammingType::Scramble)));
        assert_eq!(godma(&mut rx, scripted), vec![Some(Value::Int(1))]);
        assert!(test_world.after(lowsec, 6_500).await);
        assert!(fixture::completed_cycles(scripted) >= 1, "no real six-second cycle boundary");
        assert!(combat::state().active.contains_key(&scripted), "activation retired before deactivate");
        lifecycle::deactivate(&identity, &test_db, &test_world, &sde, Some(lowsec), tx.clone(), src_acct, scripted, "warpDisruptSphere", |_| Ok(())).await.unwrap();
        assert!(test_world.after(lowsec, 300).await);
        assert!(test_world.with(lowsec, move |s| s.projections_on(tgt_ship).is_empty()).await.unwrap());
        assert!(test_world.after(lowsec, 6_500).await);
        assert!(!combat::state().active.contains_key(&scripted));
        assert_eq!(godma(&mut rx, scripted), vec![Some(Value::Int(0))]);
        let fit = Fit::load_in_space(&test_db, &test_world, lowsec, tgt_char, tgt_ship).await.unwrap();
        assert_eq!(fit.value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS), 0.0);
        assert!(crate::services::ship::warp_pilot(&test_db, &test_world, lowsec, tgt_char, tgt_ship, destination, None, None).await.unwrap());
    }).await;
    set_locked_target(source.character, target.ship, false);
    fixture::clear_cycles(scripted);
    {
        let mut state = combat::state();
        for item in [scripted, bare, mwd_item] { state.active.remove(&item); state.activation_generation.remove(&item); }
    }
    cleanup_ship(&db, Some(&world), source).await.unwrap();
    cleanup_ship(&db, Some(&world), target).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn focused_script_projects_scram_with_script_range_and_six_second_cycle() {
    focused(45010, EFFECT_FOCUSED_WARP_SCRAMBLING_SCRIPT, 12_800.0, JammingType::Scramble).await;
    focused(29003, EFFECT_FOCUSED_WARP_DISRUPTION_SCRIPT, 24_000.0, JammingType::Disrupt).await;
}
