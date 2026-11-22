//! Database-backed scanner and tractor cycles through the system actor.
use super::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use infinity_core::{consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_CAPACITOR_NEED, dogma::ATTR_MAX_LOCKED_TARGETS, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_MAX_TRACTOR_VELOCITY, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_MODULE, CATEGORY_SHIP, CARGO_CONTAINER_GROUPS, dogma::EFFECT_CARGO_SCAN, dogma::EFFECT_SHIP_SCAN, dogma::EFFECT_SURVEY_SCAN, dogma::EFFECT_TRACTOR_BEAM, FLAG_CARGO, FLAG_HI_SLOT_FIRST, GROUP_WRECK}};
use crate::{dao::items::dogma, services::{combat::{self, ActiveModule, Ctx, groups_health, targeting_charges}, effects::{lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module}, registry, scan, tractor}, fit::Fit}, space::{World, MODE_FOLLOW, MODE_STOP}};
use marshal::Value;
use std::sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}};

fn module_type(sde: &crate::sde::Sde, effect: i32) -> i32 {
    sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == effect)).expect("module for effect").type_id
}

fn cycle(cx: &Ctx, module_id: i64, character: i64, module_type: i32, effect: registry::Effect, target: Option<i64>) -> Cycle {
    let generation = Arc::new(AtomicBool::new(false));
    {
        let mut state = combat::state();
        state.active.insert(module_id, ActiveModule { char_id: character, stop: generation.clone(), forced_inactive: false });
        state.activation_generation.insert(module_id, generation.clone());
    }
    Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }], presented_effect: effect.id, effect, target, repeat: 1,
        stop: generation.clone(), owner: ActivationOwner { master: module_id, generation, armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 }, started: 0, charge_type: None,
        cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
        source_membership: None, target_type: None, inputs: crate::services::effects::parameters::CycleInputs::default() }
}

async fn lock(cx: &Ctx, target: i64) {
    targeting_charges::add_target_ctx(cx, target, false).await.ok().expect("lock request");
    for _ in 0..100 {
        if targeting_charges::targets_of(cx.char_id).contains(&target) { return; }
        assert!(cx.world.after(cx.system_id, 100).await);
    }
    panic!("target lock did not complete");
}

async fn scanner(effect_id: i32) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v > 0.0) && t.attribute(infinity_core::consts::dogma::ATTR_RECHARGE_RATE).is_some_and(|v| v > 10000.0)).expect("capacitor hull").type_id;
    let module = module_type(&sde, effect_id);
    let effect = registry::resolve(&sde, effect_id).expect("scanner effect");
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: module, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let foreign = match create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[
        ShipItemSpec { type_id: module, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: hull, flag_id: FLAG_CARGO, quantity: 7, singleton: false, online: false, attributes: &[] },
    ]).await {
        Ok(foreign) => foreign,
        Err(e) => { cleanup_ship(&db, None, f).await.unwrap(); panic!("foreign fixture: {e}"); }
    };
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        dogma::set_override(&test_db, f.ship, ATTR_MAX_TARGET_RANGE, 10000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_SCAN_RESOLUTION, 1000000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0).await.unwrap();
        dogma::set_override(&test_db, module_id, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        fixture::admit(&test_db, &test_world, 1, fixture::ball(foreign.ship, hull, foreign.character, None, 100.0)).await;
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let target = (effect_id != EFFECT_SURVEY_SCAN).then_some(foreign.ship);
        let mut cycle = cycle(&cx, module_id, f.character, module, effect, target);
        if let Some(target) = target {
            assert!(scan::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, Some(target)).await.is_err());
            lock(&cx, target).await;
        }
        scan::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, target).await.ok().expect("valid scan");
        cycle = fixture::capture_cycle(cycle).await;
        assert_eq!(scan::start(&cx, &mut cycle).await.ok().unwrap(), CycleOutcome::Continue);
        while rx.try_recv().is_ok() {}
        if effect_id == EFFECT_SHIP_SCAN {
            let cap = groups_health::capacitor_snapshot(&test_db, &test_world, 1, foreign.ship).await.ok().flatten().unwrap();
            assert!(groups_health::tests::drain_exact(&test_db, &test_world, 1, foreign.ship, cap.capacity / 2.0).await.ok().unwrap() > 0.0);
        }
        assert_eq!(scan::end(&cx, &mut cycle).await.ok().unwrap(), CycleOutcome::Ended { reason: "scan complete", outcome: crate::lua::events::ModuleCycleOutcome::Completed });
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        match effect_id {
            EFFECT_CARGO_SCAN => {
                let expected = vec![Value::Int(foreign.ship), Value::list([Value::tuple([Value::Int(i64::from(hull)), Value::Int(7)])])];
                assert_eq!(events.iter().filter(|(name, _)| name == "OnCargoScanComplete").map(|(_, args)| args).collect::<Vec<_>>(), vec![&expected]);
            }
            EFFECT_SHIP_SCAN => {
                let scans: Vec<_> = events.iter().filter(|(name, _)| name == "OnShipScanCompleted").collect();
                assert_eq!(scans.len(), 1);
                let args = &scans[0].1;
                assert_eq!(args[0], Value::Int(foreign.ship));
                let (Value::Float(charge), Value::Float(capacity)) = (&args[1], &args[2]) else { panic!("capacitor values") };
                assert!(*charge > 0.0 && *charge < *capacity);
                assert_eq!(args[3], Value::list([Value::tuple([Value::Int(i64::from(module)), Value::Int(1)])]));
            }
            _ => assert!(events.is_empty(), "survey completion must not push a result"),
        }
        if target.is_some() {
            while rx.try_recv().is_ok() {}
            combat::state().active.get_mut(&module_id).unwrap().stop = Arc::new(AtomicBool::new(false));
            assert!(!cycle.stop.load(Ordering::Relaxed));
            assert_eq!(scan::end(&cx, &mut cycle).await.ok().unwrap(), CycleOutcome::Stop("scan complete"));
            assert!(rx.try_recv().is_err(), "stale scan must not notify");
            combat::state().active.get_mut(&module_id).unwrap().stop = cycle.stop.clone();
        }
        cycle = fixture::capture_cycle(cycle).await;
        assert_eq!(scan::start(&cx, &mut cycle).await.unwrap(), CycleOutcome::Continue);
        assert_eq!(scan::end(&cx, &mut cycle).await.unwrap(), CycleOutcome::Ended {
            reason: "scan complete", outcome: crate::lua::events::ModuleCycleOutcome::Completed });
        let second: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        match effect_id {
            EFFECT_CARGO_SCAN => assert_eq!(second.iter().filter(|(name, _)| name == "OnCargoScanComplete")
                .collect::<Vec<_>>(), events.iter().filter(|(name, _)| name == "OnCargoScanComplete").collect::<Vec<_>>()),
            EFFECT_SHIP_SCAN => {
                let scans: Vec<_> = second.iter().filter(|(name, _)| name == "OnShipScanCompleted").collect();
                assert_eq!(scans.len(), 1);
                let args = &scans[0].1;
                assert_eq!(args[0], Value::Int(foreign.ship));
                assert_eq!(args[3], Value::list([Value::tuple([Value::Int(i64::from(module)), Value::Int(1)])]));
                let (Value::Float(charge), Value::Float(capacity)) = (&args[1], &args[2]) else { panic!("capacitor values") };
                assert!(*charge > 0.0 && *charge < *capacity);
            }
            _ => assert!(second.is_empty()),
        }
        fixture::set_cycle_online(&cycle, f.account, false).await;
        while rx.try_recv().is_ok() {}
        assert!(matches!(scan::start(&cx, &mut cycle).await.unwrap(), CycleOutcome::Stop(_)));
        if target.is_some() { assert_eq!(scan::end(&cx, &mut cycle).await.unwrap(), CycleOutcome::Stop("scan complete")); }
        assert!(rx.try_recv().is_err(), "offline source produced a result");
        let stop_cx = cx.clone();
        test_world.with(1, move |s| scan::stop(s, &stop_cx, &cycle)).await.unwrap();
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    targeting_charges::clear_locks_on(&world, 1, f.ship, f.character);
    for id in [f.ship, foreign.ship] { world.remove_ball(1, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db,
        &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character },
            crate::services::tests::fixture::Fixture { account: foreign.account, character: foreign.character }],
        &[f.ship, module_id, foreign.ship, foreign.module()]).await.unwrap();
    let cleanup_foreign = cleanup_ship(&db, None, foreign).await;
    let cleanup_source = cleanup_ship(&db, None, f).await;
    cleanup_foreign.unwrap(); cleanup_source.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn cargo_scanner_reads_foreign_cargo_once() { scanner(EFFECT_CARGO_SCAN).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn ship_scanner_reads_foreign_fitting_and_live_capacitor_once() { scanner(EFFECT_SHIP_SCAN).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn survey_cycle_finishes_without_inventing_a_result_event() { scanner(EFFECT_SURVEY_SCAN).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn tractor_owns_and_moves_container_then_restores_motion() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).expect("hull").type_id;
    let module = module_type(&sde, EFFECT_TRACTOR_BEAM);
    let container = sde.types.iter().find(|t| t.group_id.is_some_and(|g| g == GROUP_WRECK || CARGO_CONTAINER_GROUPS.contains(&g))).expect("container").type_id;
    let effect = registry::resolve(&sde, EFFECT_TRACTOR_BEAM).expect("tractor effect");
    let mut f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: module, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let world = World::new(fixture::cycle_observer(db.clone()), sde.clone(), 1);
    let target_id = Arc::new(AtomicI64::new(0));
    let test_target_id = target_id.clone();
    let test_world = world.clone(); let test_db = db.clone();
    let result = tokio::spawn(async move {
        let target = fixture::item(&test_db, container, f.character, 1, 0, 1, true, false).await.unwrap();
        test_target_id.store(target, Ordering::Relaxed);
        fixture::position(&test_db, target, 1, [100.0, 0.0, 0.0]).await.unwrap();
        for (item, attr, value) in [(f.ship, ATTR_MAX_TARGET_RANGE, 10000.0), (f.ship, ATTR_SCAN_RESOLUTION, 1000000.0), (f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0), (module_id, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0), (module_id, ATTR_MAX_TRACTOR_VELOCITY, 100.0), (module_id, effect.attribute(registry::EffectAttribute::Duration).unwrap(), 2000.0), (module_id, effect.attribute(registry::EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED), 0.0)] {
            dogma::set_override(&test_db, item, attr, value).await.unwrap();
        }
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        let mut ball = fixture::ball(target, container, f.character, None, 100.0);
        ball.free = true; ball.max_vel = 12.0;
        test_world.add_ship(1, ball).await.unwrap();
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let mut cycle = self::cycle(&cx, module_id, f.character, module, effect.clone(), Some(target));
        assert!(tractor::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, Some(target)).await.is_err());
        lock(&cx, target).await;
        tractor::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, Some(target)).await.ok().expect("valid tractor");
        cycle = fixture::capture_cycle(cycle).await;
        assert_eq!(tractor::start(&cx, &mut cycle).await.ok().unwrap(), CycleOutcome::Continue);
        let other = Arc::new(AtomicBool::new(false));
        assert!(!test_world.with(1, move |s| s.begin_tractor(target, -module_id, other, 10.0, 0.0, f.ship)).await.unwrap());
        assert!(test_world.after(1, 1500).await);
        let moving = test_world.ball(1, target).await.unwrap();
        assert_eq!(moving.mode, MODE_FOLLOW); assert_eq!(moving.follow_id, f.ship);
        assert_eq!(moving.max_vel, 100.0); assert!(moving.pos[0] < 100.0);
        let stop_cx = cx.clone();
        test_world.with(1, move |s| tractor::stop(s, &stop_cx, &cycle)).await.unwrap();
        assert!(test_world.after(1, 1500).await);
        let restored = test_world.ball(1, target).await.unwrap();
        assert_eq!(restored.mode, MODE_STOP); assert_eq!(restored.max_vel, 12.0); assert_eq!(restored.speed_fraction, 0.0);
        let mut repeated = self::cycle(&cx, module_id, f.character, module, effect.clone(), Some(target));
        repeated.repeat = 2;
        repeated = fixture::capture_cycle(repeated).await;
        fixture::clear_cycles(module_id);
        test_world.with(1, move |s| {
            let now = s.now_ms();
            crate::services::effects::lifecycle::cycle_start(s, repeated, now);
        }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while fixture::completed_cycles(module_id) < 2 { assert!(test_world.after(1, 100).await); }
        }).await.expect("two scheduled tractor cycles");
        assert_eq!(fixture::completed_cycles(module_id), 2);
        let mut rejected = self::cycle(&cx, module_id, f.character, module, effect, Some(target));
        rejected = fixture::capture_cycle(rejected).await;
        fixture::set_cycle_online(&rejected, f.account, false).await;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let ball = test_world.ball(1, target).await.unwrap();
                if (ball.mode, ball.max_vel, ball.follow_id, ball.speed_fraction) == (MODE_STOP, 12.0, 0, 0.0) {
                    break;
                }
                assert!(test_world.after(1, 100).await);
            }
        }).await.expect("tractor baseline was not restored");
        let before_rejected = test_world.ball(1, target).await.unwrap();
        assert!(matches!(tractor::start(&cx, &mut rejected).await.unwrap(), CycleOutcome::Stop(_)));
        assert!(test_world.after(1, 1200).await);
        let after_rejected = test_world.ball(1, target).await.unwrap();
        assert_eq!((after_rejected.mode, after_rejected.max_vel, after_rejected.follow_id),
            (before_rejected.mode, before_rejected.max_vel, before_rejected.follow_id));
        assert_eq!(fixture::completed_cycles(module_id), 2);
        fixture::clear_cycles(module_id);
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    targeting_charges::clear_locks_on(&world, 1, f.ship, f.character);
    let target = target_id.load(Ordering::Relaxed);
    if target != 0 { world.remove_ball(1, target).await; combat::forget_ship(target); f.items.push(target); }
    world.remove_ball(1, f.ship).await; combat::forget_ship(f.ship);
    world.unregister_pilot_generation(f.character, None);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id, target]).await.unwrap();
    let flush = world.flush_positions().await; drop(world);
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap(); result.unwrap();
}
