//! Native player remote-repair admission and checkpoint regressions.
use super::*;
use crate::services::combat::tests::remote_repair_db as fixture;
use crate::services::effects::behaviors::on_cycle_start;
use infinity_core::consts::dogma::ATTR_CAPACITOR_NEED;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_FITTING_FIRST,
    dogma::{ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE,
        ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, EFFECT_REMOTE_SHIELD_BOOSTER,
        EFFECT_REMOTE_ARMOR_REPAIRER, EFFECT_REMOTE_HULL_REPAIRER}};
use crate::lua::events::combat::{BinaryAmount, HpLayer};
use crate::space::combat::RepairSource;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn player_remote_phases_clamping_frozen_source_and_cancel() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE,
            ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE].iter().all(|&a| t.attribute(a).is_some_and(|v| v > 10.0))).unwrap().type_id;
    for (effect_id, layer, expected_layer) in [(EFFECT_REMOTE_SHIELD_BOOSTER, 0, HpLayer::Shield),
        (EFFECT_REMOTE_ARMOR_REPAIRER, 1, HpLayer::Armor), (EFFECT_REMOTE_HULL_REPAIRER, 2, HpLayer::Hull)] {
        let effect = registry::resolve(&sde, effect_id).unwrap();
        let amount_attr = super::super::super::repair::amount_attribute(layer);
        let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| e.effect_id == effect_id)
            && t.attribute(amount_attr).is_some_and(|v| v > 0.0)).unwrap().type_id;
        let f = fixture::create(&db, ship_type, module_type, 1).await.unwrap();
        crate::dao::items::dogma::set_override(&db, f.module, amount_attr, 1e30).await.unwrap();
        let world = World::new(db.clone(), sde.clone(), 1);
        let (notify, _rx) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(f.source, notify.clone());
        world.add_ship(1, fixture::ball(f.ship, ship_type, f.source)).await.unwrap();
        world.add_ship(1, fixture::ball(f.victim, ship_type, f.target)).await.unwrap();
        let fit = Fit::load_in_space(&db, &world, 1, f.source, f.ship).await.unwrap();
        fixture::install_half(&world, 1, f.ship, Fit::load_in_space(&db, &world, 1, f.source, f.ship).await.unwrap()).await;
        fixture::install_half(&world, 1, f.victim, Fit::load_in_space(&db, &world, 1, f.target, f.victim).await.unwrap()).await;
        crate::services::combat::damage::repair_target(&world, 1, f.victim, layer, 1.0, RepairSource::Local, |_| true).await.unwrap();
        assert!(world.with(1, |s| crate::space::snapshot::capture_fact_checkpoint(s).unwrap().is_none()).await.unwrap());
        let before = fixture::condition(&world, 1, f.victim).await;
        let stop = Arc::new(AtomicBool::new(false));
        state().active.insert(f.module, ActiveModule { char_id: f.source, stop: stop.clone(), forced_inactive: false });
        state().activation_generation.insert(f.module, stop.clone());
        fixture::set_locked_target(f.source, f.victim, true);
        let cx = Ctx { db: db.clone(), world: world.clone(), notify, char_id: f.source, ship_id: f.ship, system_id: 1 };
        let generation = crate::space::targeting::next_generation();
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: module_type, flag: FLAG_FITTING_FIRST }],
            effect, target: Some(f.victim), repeat: 1, stop: stop.clone(),
            owner: ActivationOwner { master: f.module, generation: stop, armed: true,
                combat_generations: vec![NativeCombatGeneration { module_id: f.module, generation, sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&world), system: 1 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0,
            other: None, repair_amount: 0.0, illegal: false };
        if layer != 0 {
            assert!(matches!(on_cycle_start(&cx, &fit, &mut cycle, false).await.unwrap(), CycleOutcome::Continue));
            let cancelled = cycle.owner.pending_combat.as_ref().unwrap().facts();
            fixture::set_locked_target(f.source, f.victim, false);
            assert!(matches!(on_cycle_end(&cx, &fit, &mut cycle).await.unwrap(), CycleOutcome::Stop(_) | CycleOutcome::Ended { .. }));
            assert!(cycle.owner.pending_combat.is_none());
            assert_eq!(fixture::condition(&world, 1, f.victim).await.now[layer], before.now[layer]);
            assert!(world.with(1, move |s| s.prepare_combat_source(f.ship, Some(f.module), Some(i64::from(effect_id)), None,
                crate::lua::events::resources::FactOrigin::Natural, generation, cancelled.sequence, 1, |_| true)).await.unwrap().is_err());
            fixture::set_locked_target(f.source, f.victim, true);
        }
        assert!(matches!(on_cycle_start(&cx, &fit, &mut cycle, false).await.unwrap(), CycleOutcome::Continue));
        let after_start = fixture::condition(&world, 1, f.victim).await;
        if layer == 0 { assert_eq!(after_start.now[layer], before.stats.max[layer]); }
        else {
            assert_eq!(after_start.now[layer], before.now[layer]);
            world.with(1, move |s| s.refresh_pilot_affiliation(f.source, f.ship, Some(2), Some(3))).await.unwrap();
            assert!(matches!(on_cycle_end(&cx, &fit, &mut cycle).await.unwrap(), CycleOutcome::Continue));
        }
        let after = fixture::condition(&world, 1, f.victim).await;
        assert_eq!(after.now[layer], before.stats.max[layer]);
        let frame = world.with(1, |s| crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()).await.unwrap();
        let events: Vec<_> = frame.events().map(|captured| captured.event).collect();
        let [GameEvent::RemoteRepairApplied { source, target, module_id, effect_id: actual_effect, layer: actual_layer, slices, .. }] = events.as_slice() else { panic!("expected one remote repair fact") };
        assert_eq!(source.character_id, Some(f.source));
        assert_eq!(source.corporation_id, Some(1));
        assert_eq!(target.item_id, f.victim);
        assert_eq!(*module_id, Some(f.module));
        assert_eq!(*actual_effect, Some(i64::from(effect_id)));
        assert_eq!(*actual_layer, expected_layer);
        let before_repair = if layer == 0 {
            let dt = after.shield_at.saturating_sub(before.shield_at).max(0) as f64 / infinity_core::consts::FILETIME_SECOND as f64;
            crate::services::combat::groups_health::recharge(before.now[0], before.stats.max[0], before.stats.shield_tau_ms, dt)
        } else { before.now[layer] };
        assert_eq!(slices[0].amount, BinaryAmount::from_f64(after.now[layer] - before_repair).unwrap());
        if layer != 0 {
            assert!(matches!(on_cycle_start(&cx, &fit, &mut cycle, false).await.unwrap(), CycleOutcome::Continue));
            assert!(cycle.owner.pending_combat.is_some());
        }
        world.with(1, move |s| cycle_stop(s, cycle)).await.unwrap();
        assert_eq!(fixture::condition(&world, 1, f.victim).await.now[layer], after.now[layer]);
        fixture::set_locked_target(f.source, f.victim, false);
        world.remove_ball(1, f.ship).await;
        world.remove_ball(1, f.victim).await;
        world.unregister_pilot_generation(f.source, None);
        world.flush_positions().await.unwrap();
        crate::services::combat::forget_ship(f.ship);
        crate::services::combat::forget_ship(f.victim);
        drop(world);
        db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[f.module, f.ship, f.victim][..]]).await.unwrap();
        db.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::services::tests::fixture::cleanup_progress(tx, &[f.account, f.source, f.target], &[f.source, f.target]).await
        }), |(), ()| async { Ok(()) }).await.unwrap();
        fixture::cleanup(&db, f).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn full_remote_repair_refuses_and_driver_retires_activation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, EFFECT_REMOTE_SHIELD_BOOSTER).unwrap();
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_REMOTE_SHIELD_BOOSTER)
        && t.attribute(infinity_core::consts::dogma::ATTR_SHIELD_BONUS).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP, ATTR_SHIELD_RECHARGE_RATE,
            ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE].iter().all(|&a| t.attribute(a).is_some_and(|v| v > 10.0))).unwrap().type_id;
    let f = fixture::create(&db, ship_type, module_type, 1).await.unwrap();
    let discharge = effect.attribute(EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED);
    crate::dao::items::dogma::set_override(&db, f.module, discharge, 0.0).await.unwrap();
    let world = World::new(db.clone(), sde, 1);
    let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(f.source, notify.clone());
    world.add_ship(1, fixture::ball(f.ship, ship_type, f.source)).await.unwrap();
    world.add_ship(1, fixture::ball(f.victim, ship_type, f.target)).await.unwrap();
    let source_fit = Fit::load_in_space(&db, &world, 1, f.source, f.ship).await.unwrap();
    let target_fit = Fit::load_in_space(&db, &world, 1, f.target, f.victim).await.unwrap();
    fixture::install_half(&world, 1, f.ship, source_fit).await;
    fixture::install_half(&world, 1, f.victim, target_fit).await;
    world.with(1, |s| crate::space::snapshot::facts_test_init(s, 0, 1)).await.unwrap();
    let before = fixture::condition(&world, 1, f.victim).await.now;
    let stop = Arc::new(AtomicBool::new(false));
    state().active.insert(f.module, ActiveModule { char_id: f.source, stop: stop.clone(), forced_inactive: false });
    state().activation_generation.insert(f.module, stop.clone());
    fixture::set_locked_target(f.source, f.victim, true);
    let cx = Ctx { db: db.clone(), world: world.clone(), notify, char_id: f.source, ship_id: f.ship, system_id: 1 };
    let cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: module_type, flag: FLAG_FITTING_FIRST }],
        effect: effect.clone(), target: Some(f.victim), repeat: 1, stop: stop.clone(),
        owner: ActivationOwner { master: f.module, generation: stop.clone(), armed: true,
            combat_generations: vec![NativeCombatGeneration { module_id: f.module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
            pending_combat: None, world: Arc::downgrade(&world), system: 1 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0,
        other: None, repair_amount: 0.0, illegal: false };
    begin::cycle_begin_work(cycle).await;
    world.with(1, |_| ()).await.unwrap();
    assert_eq!(fixture::condition(&world, 1, f.victim).await.now, before);
    assert!(!state().active.contains_key(&f.module));
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|(event, args)| event == "OnRemoteMessage"
        && args.first() == Some(&Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY))));
    let replacement = Arc::new(AtomicBool::new(false));
    state().active.insert(f.module, ActiveModule { char_id: f.source, stop: replacement.clone(), forced_inactive: false });
    state().activation_generation.insert(f.module, replacement.clone());
    let stale = Cycle { cx, modules: vec![Module { id: f.module, type_id: module_type, flag: FLAG_FITTING_FIRST }],
        effect, target: Some(f.victim), repeat: 1, stop: stop.clone(),
        owner: ActivationOwner { master: f.module, generation: stop, armed: true,
            combat_generations: Vec::new(), pending_combat: None, world: Arc::downgrade(&world), system: 1 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0,
        other: None, repair_amount: 0.0, illegal: false };
    world.with(1, move |s| cycle_stop(s, stale)).await.unwrap();
    assert!(state().active.get(&f.module).is_some_and(|active| Arc::ptr_eq(&active.stop, &replacement)));
    state().active.remove(&f.module);
    state().activation_generation.remove(&f.module);
    fixture::set_locked_target(f.source, f.victim, false);
    world.remove_ball(1, f.ship).await;
    world.remove_ball(1, f.victim).await;
    world.unregister_pilot_generation(f.source, None);
    world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(f.ship);
    crate::services::combat::forget_ship(f.victim);
    drop(world);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[f.module, f.ship, f.victim][..]]).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[f.account, f.source, f.target], &[f.source, f.target]).await
    }), |(), ()| async { Ok(()) }).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
