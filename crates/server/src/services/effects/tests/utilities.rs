//! Database-backed scanner and tractor cycles through the system actor.
use super::fixture;
use infinity_core::{consts::{dogma::ATTR_CAPACITOR_CAPACITY, dogma::ATTR_MAX_LOCKED_TARGETS, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_MAX_TRACTOR_VELOCITY, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_MODULE, CATEGORY_SHIP, CARGO_CONTAINER_GROUPS, dogma::EFFECT_CARGO_SCAN, dogma::EFFECT_SHIP_SCAN, dogma::EFFECT_SURVEY_SCAN, dogma::EFFECT_TRACTOR_BEAM, FLAG_CARGO, FLAG_HI_SLOT_FIRST, GROUP_WRECK}};
use crate::{dao::items::dogma, services::{combat::{self, ActiveModule, Ctx, groups_health, targeting_charges}, effects::{lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module}, registry, scan, tractor}, fit::Fit}, space::{World, MODE_FOLLOW, MODE_STOP}};
use marshal::Value;
use std::sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}};

fn module_type(sde: &crate::sde::Sde, effect: i32) -> i32 {
    sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == effect)).expect("module for effect").type_id
}

fn cycle(cx: &Ctx, f: fixture::Fixture, module_type: i32, effect: registry::Effect, target: Option<i64>) -> Cycle {
    let generation = Arc::new(AtomicBool::new(false));
    {
        let mut state = combat::state();
        state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
        state.activation_generation.insert(f.module, generation.clone());
    }
    Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }], effect, target, repeat: 1,
        stop: generation.clone(), owner: ActivationOwner { master: f.module, generation, armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 }, started: 0, charge_type: None,
        cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false }
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
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_HI_SLOT_FIRST).await.unwrap();
    let foreign = match fixture::create(&db, hull, module, FLAG_HI_SLOT_FIRST).await {
        Ok(foreign) => foreign,
        Err(e) => { fixture::cleanup(&db, f).await.unwrap(); panic!("foreign fixture: {e}"); }
    };
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        dogma::set_override(&test_db, f.ship, ATTR_MAX_TARGET_RANGE, 10000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_SCAN_RESOLUTION, 1000000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0).await.unwrap();
        dogma::set_override(&test_db, f.module, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0).await.unwrap();
        fixture::item(&test_db, hull, foreign.character, foreign.ship, FLAG_CARGO, 7, false, false).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        test_world.add_ship(1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await.unwrap();
        test_world.add_ship(1, fixture::ball(foreign.ship, hull, foreign.character, None, 100.0)).await.unwrap();
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let target = (effect_id != EFFECT_SURVEY_SCAN).then_some(foreign.ship);
        let mut cycle = cycle(&cx, f, module, effect, target);
        if let Some(target) = target {
            assert!(scan::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, Some(target)).await.is_err());
            lock(&cx, target).await;
        }
        scan::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, target).await.ok().expect("valid scan");
        assert_eq!(scan::start(&cx, &fit, &mut cycle).await.ok().unwrap(), CycleOutcome::Continue);
        while rx.try_recv().is_ok() {}
        if effect_id == EFFECT_SHIP_SCAN {
            let cap = groups_health::capacitor_snapshot(&test_db, &test_world, 1, foreign.ship).await.ok().flatten().unwrap();
            assert!(groups_health::tests::drain_exact(&test_db, &test_world, 1, foreign.ship, cap.capacity / 2.0).await.ok().unwrap() > 0.0);
        }
        assert_eq!(scan::end(&cx, &fit, &mut cycle).await.ok().unwrap(), CycleOutcome::Ended { reason: "scan complete", outcome: crate::lua::events::ModuleCycleOutcome::Completed });
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
            combat::state().active.get_mut(&f.module).unwrap().stop = Arc::new(AtomicBool::new(false));
            assert!(!cycle.stop.load(Ordering::Relaxed));
            assert_eq!(scan::end(&cx, &fit, &mut cycle).await.ok().unwrap(), CycleOutcome::Stop("scan complete"));
            assert!(rx.try_recv().is_err(), "stale scan must not notify");
            combat::state().active.get_mut(&f.module).unwrap().stop = cycle.stop.clone();
        }
        let stop_cx = cx.clone();
        test_world.with(1, move |s| scan::stop(s, &stop_cx, &cycle)).await.unwrap();
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    targeting_charges::clear_locks_on(&world, 1, f.ship, f.character);
    for id in [f.ship, foreign.ship] { world.remove_ball(1, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    let cleanup_foreign = fixture::cleanup(&db, foreign).await;
    let cleanup_source = fixture::cleanup(&db, f).await;
    cleanup_foreign.unwrap(); cleanup_source.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
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
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let target_id = Arc::new(AtomicI64::new(0));
    let test_target_id = target_id.clone();
    let test_world = world.clone(); let test_db = db.clone();
    let result = tokio::spawn(async move {
        let target = fixture::item(&test_db, container, f.character, 1, 0, 1, true, false).await.unwrap();
        test_target_id.store(target, Ordering::Relaxed);
        fixture::position(&test_db, target, 1, [100.0, 0.0, 0.0]).await.unwrap();
        for (item, attr, value) in [(f.ship, ATTR_MAX_TARGET_RANGE, 10000.0), (f.ship, ATTR_SCAN_RESOLUTION, 1000000.0), (f.ship, ATTR_MAX_LOCKED_TARGETS, 10.0), (f.module, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0), (f.module, ATTR_MAX_TRACTOR_VELOCITY, 100.0)] {
            dogma::set_override(&test_db, item, attr, value).await.unwrap();
        }
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        test_world.add_ship(1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await.unwrap();
        let mut ball = fixture::ball(target, container, f.character, None, 100.0);
        ball.free = true; ball.max_vel = 12.0;
        test_world.add_ship(1, ball).await.unwrap();
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let mut cycle = cycle(&cx, f, module, effect, Some(target));
        assert!(tractor::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, Some(target)).await.is_err());
        lock(&cx, target).await;
        tractor::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, Some(target)).await.ok().expect("valid tractor");
        assert_eq!(tractor::start(&cx, &fit, &mut cycle).await.ok().unwrap(), CycleOutcome::Continue);
        let other = Arc::new(AtomicBool::new(false));
        assert!(!test_world.with(1, move |s| s.begin_tractor(target, -f.module, other, 10.0, 0.0, f.ship)).await.unwrap());
        assert!(test_world.after(1, 1500).await);
        let moving = test_world.ball(1, target).await.unwrap();
        assert_eq!(moving.mode, MODE_FOLLOW); assert_eq!(moving.follow_id, f.ship);
        assert_eq!(moving.max_vel, 100.0); assert!(moving.pos[0] < 100.0);
        let stop_cx = cx.clone();
        test_world.with(1, move |s| tractor::stop(s, &stop_cx, &cycle)).await.unwrap();
        assert!(test_world.after(1, 1500).await);
        let restored = test_world.ball(1, target).await.unwrap();
        assert_eq!(restored.mode, MODE_STOP); assert_eq!(restored.max_vel, 12.0); assert_eq!(restored.speed_fraction, 0.0);
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    targeting_charges::clear_locks_on(&world, 1, f.ship, f.character);
    let target = target_id.load(Ordering::Relaxed);
    if target != 0 { world.remove_ball(1, target).await; combat::forget_ship(target); }
    world.remove_ball(1, f.ship).await; combat::forget_ship(f.ship);
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await; drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap(); result.unwrap();
}
