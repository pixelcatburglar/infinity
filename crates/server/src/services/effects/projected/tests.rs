use super::*;
use crate::services::fit::{OP_MOD_ADD, OP_POST_PERCENT};


#[test]
fn falloff_scales_operands_around_operation_identity() {
    assert_eq!(scaled(-60.0, OP_POST_PERCENT, 0.5), -30.0);
    assert_eq!(scaled(4.0, OP_MOD_ADD, 0.5), 2.0);
    assert_eq!(scaled(0.4, OP_POST_MUL, 0.5), 0.7);
    assert_eq!(scaled(2.0, OP_POST_DIV, 0.0), 1.0);
    assert_eq!(scaled(2.0, OP_PRE_MUL, 1.0), 2.0);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn player_scrambler_lands_on_aligning_ship_before_warp_activation() {
    use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_FITTING_FIRST,
        dogma::{ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_SPEED_FACTOR, ATTR_WARP_SCRAMBLE_STATUS, EFFECT_MICROWARPDRIVE, EFFECT_WARP_SCRAMBLE}};
    use crate::services::combat::{self, ActiveModule, Ctx};
    use crate::services::combat::tests::remote_repair_db as fixture;
    use crate::services::effects::{behaviors::{on_activate, on_cycle_start, Activation}, lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module, NativeCombatGeneration}, registry};
    use marshal::Value;
    use std::sync::atomic::{AtomicBool, Ordering};

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, EFFECT_WARP_SCRAMBLE).unwrap();
    let module_type = sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_WARP_SCRAMBLE))
        .expect("published scrambler").type_id;
    let source_fit = Fit::for_space_type(sde.clone(), module_type, vec![]);
    let (values, _) = snapshot(&sde, &source_fit, Key::Ship, &effect, None);
    let projection = Projection { source_ship: -1, source_module: -2, source_type: module_type,
        effect_id: EFFECT_WARP_SCRAMBLE, kind: effect.kind, until_ms: i64::MAX,
        values: values.into_iter().map(|(id, value, _)| (id, value)).collect(),
        charge_values: vec![], charge_type: None, scramble: None };
    let boost_attr = sde.dogma.attributes_by_name("speedBoostFactor").next().unwrap().attribute_id;
    let mwd_effect = registry::resolve(&sde, EFFECT_MICROWARPDRIVE).unwrap();
    let duration_attr = mwd_effect.attribute(registry::EffectAttribute::Duration).unwrap();
    let mwd_type = sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICROWARPDRIVE)
        && t.fit_attribute(ATTR_SPEED_FACTOR).is_some_and(|v| v > 0.0)
        && t.fit_attribute(boost_attr).is_some_and(|v| v > 0.0)
        && t.fit_attribute(duration_attr).is_some_and(|v| (10_000.0..=30_000.0).contains(&v)))
        .expect("published microwarpdrive with thrust").type_id;
    let mwd_name = sde.dogma.effect(EFFECT_MICROWARPDRIVE).unwrap().name.clone().unwrap();
    let ship_type = sde.types.iter().filter(|t| t.published == Some(true)
        && t.category_id == Some(CATEGORY_SHIP)
        && [infinity_core::consts::dogma::ATTR_SHIELD_CAPACITY, infinity_core::consts::dogma::ATTR_ARMOR_HP,
            infinity_core::consts::dogma::ATTR_STRUCTURE_HP, infinity_core::consts::dogma::ATTR_SHIELD_RECHARGE_RATE,
            infinity_core::consts::dogma::ATTR_CAPACITOR_CAPACITY, infinity_core::consts::dogma::ATTR_RECHARGE_RATE]
            .iter().all(|&a| t.attribute(a).is_some_and(|v| v > 10.0))
        && t.fit_attribute(ATTR_MASS).is_some_and(|v| v > 0.0)
        && t.fit_attribute(ATTR_MAX_VELOCITY).is_some_and(|v| v > 0.0)
        && Fit::for_space_type(sde.clone(), t.type_id, vec![]).value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) == 0.0
        && Fit::for_space_type(sde.clone(), t.type_id, vec![projection.clone()]).value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS) > 0.0)
        .max_by(|a, b| a.attribute(infinity_core::consts::dogma::ATTR_CAPACITOR_CAPACITY).unwrap()
            .total_cmp(&b.attribute(infinity_core::consts::dogma::ATTR_CAPACITOR_CAPACITY).unwrap()))
        .expect("ship with health and capacitor").type_id;
    let f = fixture::create(&db, ship_type, module_type, 1).await.unwrap();
    let mwd = crate::dao::items::creation::rookie_ship(&db, mwd_type, f.target, f.victim, infinity_core::consts::FLAG_MED_SLOT_FIRST).await.unwrap();
    db.execute("UPDATE game.items SET online = true WHERE item_id = $1", &[&mwd]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (notify, _rx) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(f.source, notify.clone());
        let (target_notify, _target_rx) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(f.target, target_notify.clone());
        w.add_ship(1, fixture::ball(f.ship, ship_type, f.source)).await.unwrap();
        let mut target = fixture::ball(f.victim, ship_type, f.target);
        target.free = true;
        w.add_ship(1, target).await.unwrap();
        let fit = Fit::load_in_space(&d, &w, 1, f.source, f.ship).await.unwrap();
        fixture::install_half(&w, 1, f.ship, Fit::load_in_space(&d, &w, 1, f.source, f.ship).await.unwrap()).await;
        fixture::install_half(&w, 1, f.victim, Fit::load_in_space(&d, &w, 1, f.target, f.victim).await.unwrap()).await;
        let baseline = Fit::load_in_space(&d, &w, 1, f.target, f.victim).await.unwrap().value(Key::Ship, ATTR_MAX_VELOCITY);
        let identity = crate::services::identity::Identity { char_id: f.target, corp_id: 1, ship_id: Some(f.victim), station_id: None, system_id: Some(1) };
        crate::services::effects::lifecycle::activate(&identity, &d, &w, w.sde(), Some(1),
            target_notify, f.account, mwd, &mwd_name, None, 100, |_| Ok(())).await.unwrap();
        for _ in 0..100 {
            let boosted = Fit::load_in_space(&d, &w, 1, f.target, f.victim).await.unwrap().value(Key::Ship, ATTR_MAX_VELOCITY);
            if combat::active_modules().contains(&mwd) && boosted > baseline
                && w.ball(1, f.victim).await.is_some_and(|b| f64::from(b.max_vel) > baseline) { break; }
            assert!(w.after(1, 100).await);
        }
        assert!(combat::active_modules().contains(&mwd), "real MWD activation did not remain active");
        let boosted = Fit::load_in_space(&d, &w, 1, f.target, f.victim).await.unwrap().value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!(boosted > baseline, "MWD did not boost fitted speed");
        assert!(f64::from(w.ball(1, f.victim).await.unwrap().max_vel) > baseline, "actor did not apply MWD speed");
        let (destiny_tx, mut destiny_rx) = tokio::sync::mpsc::unbounded_channel();
        w.observe(1, f.target, f.victim, destiny_tx).await.unwrap();
        // Controlled kinematics keep alignment pending after proving the real MWD boost.
        w.with(1, move |s| s.command(f.victim, |b, _, _| {
            b.max_vel = 0.0;
            b.vel = [0.0; 3];
            Vec::new()
        })).await.unwrap();
        assert!(w.after(1, 1500).await);
        while destiny_rx.try_recv().is_ok() {}
        let called = Arc::new(AtomicBool::new(false));
        let exit = called.clone();
        let guard = w.begin_warp_arrival(1, Box::new(move |_| { exit.store(true, Ordering::SeqCst); })).await.unwrap();
        let accepted = crate::services::ship::warp_pilot(&d, &w, 1, f.target, f.victim,
            crate::space::system::warp::WarpDestination::Exact([1_000_000.0, 0.0, 0.0]), Some(guard.id()), None).await.unwrap();
        assert!(accepted);
        drop(guard);
        assert!(w.after(1, 1500).await);
        assert!(w.ball(1, f.victim).await.is_some_and(|b| b.mode == crate::space::MODE_WARP && !b.is_warping()));
        assert!(combat::active_modules().contains(&mwd), "MWD stopped before scramble application");
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.source, stop: stop.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, stop.clone());
        }
        fixture::set_locked_target(f.source, f.victim, true);
        let cx = Ctx { db: d.clone(), world: w.clone(), notify, char_id: f.source, ship_id: f.ship, system_id: 1 };
        on_activate(&cx, &fit, &Activation { effect: effect.clone(),
            module: Module { id: f.module, type_id: module_type, flag: FLAG_FITTING_FIRST },
            target: Some(f.victim) }).await.unwrap();
        let generation = crate::space::targeting::next_generation();
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: module_type, flag: FLAG_FITTING_FIRST }],
            effect, target: Some(f.victim), repeat: 1, stop: stop.clone(),
            owner: ActivationOwner { master: f.module, generation: stop, armed: true,
                combat_generations: vec![NativeCombatGeneration { module_id: f.module, generation, sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&w), system: 1 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 60_000.0, nominal_start: 0,
            other: None, repair_amount: 0.0, illegal: false };
        assert!(matches!(on_cycle_start(&cx, &fit, &mut cycle, false).await.unwrap(), CycleOutcome::Continue));
        w.with(1, |_| ()).await.unwrap();
        let status = Fit::load_in_space(&d, &w, 1, f.target, f.victim).await.unwrap().value(Key::Ship, ATTR_WARP_SCRAMBLE_STATUS);
        assert!(status > 0.0, "scrambler did not overcome target resistance: {status}");
        assert!(!combat::active_modules().contains(&mwd), "scramble did not remove MWD from active Fit input");
        assert!(combat::state().active.get(&mwd).is_none_or(|active| active.forced_inactive),
            "running MWD was not forced inactive");
        let blocked_speed = Fit::load_in_space(&d, &w, 1, f.target, f.victim).await.unwrap().value(Key::Ship, ATTR_MAX_VELOCITY);
        assert!((blocked_speed - baseline).abs() <= baseline.abs().max(1.0) * 1e-10,
            "forced-inactive MWD still changes fitted speed");
        w.with(1, move |s| s.command(f.victim, |b, _, _| {
            b.max_vel = 100.0;
            b.vel = [100.0, 0.0, 0.0];
            Vec::new()
        })).await.unwrap();
        assert!(w.after(1, 1500).await);
        assert_eq!(w.ball(1, f.victim).await.unwrap().mode, crate::space::MODE_STOP, "scrambled aligning ship did not stop");
        let mut saw_stop = false;
        while let Ok(batch) = destiny_rx.try_recv() {
            saw_stop |= batch.iter().any(|v| {
                let Value::Tuple(parts) = v else { return false };
                let Value::Tuple(call) = &parts[1] else { return false };
                call[0].as_bytes() == Some(b"Stop") && call[1] == Value::Tuple(vec![Value::Int(f.victim)])
            });
        }
        assert!(saw_stop, "scramble cancellation did not publish Stop");
        assert!(!called.load(Ordering::SeqCst));
        fixture::set_locked_target(f.source, f.victim, false);
    }).await;
    if let Some(active) = combat::state().active.get(&mwd) {
        active.stop.store(true, Ordering::Relaxed);
    }
    for _ in 0..350 {
        let retired = { let state = combat::state(); !state.active.contains_key(&mwd) && !state.activation_generation.contains_key(&mwd) };
        if retired { break; }
        assert!(world.after(1, 100).await);
    }
    let mwd_retired = { let state = combat::state(); !state.active.contains_key(&mwd) && !state.activation_generation.contains_key(&mwd) };
    fixture::set_locked_target(f.source, f.victim, false);
    world.remove_ball(1, f.ship).await;
    world.remove_ball(1, f.victim).await;
    world.unregister_pilot_generation(f.source, None);
    world.unregister_pilot_generation(f.target, None);
    world.flush_positions().await.unwrap();
    combat::forget_ship(f.ship);
    combat::forget_ship(f.victim);
    drop(world);
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[f.account, f.source, f.target], &[f.source, f.target]).await
    }), |(), ()| async { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&mwd]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2 AND location_id = $3", &[&mwd, &f.target, &f.victim]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert!(mwd_retired, "MWD lifecycle did not retire during fixture cleanup");
    result.unwrap();
}
