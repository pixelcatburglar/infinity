//! Module ECM must publish Crimewatch only after final actor admission.
use super::*;
use std::{future::Future, sync::atomic::AtomicBool, task::Poll};
use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::{combat::{ActiveModule, targeting_charges}, drones::{settings, tests::{fixture::Fixture, resources::fleet::form_fleet}}, effects::{lifecycle::ActivationOwner, registry}}, space::drone::DroneOrder};

async fn catch_scenario(future: impl Future<Output = Result<()>>) -> std::thread::Result<Result<()>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
        Ok(Poll::Pending) => Poll::Pending,
        Err(error) => Poll::Ready(Err(error)),
    }).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn module_ecm_final_admission_precedes_crimewatch() -> Result<()> {
    let mut f = Fixture::new().await;
    let attacker = f.owner;
    let attacker_ship = f.ship;
    let mut victim_pair = None;
    let mut module_id = None;
    let result = catch_scenario(async {
        let (victim, victim_ship) = f.add_pilot().await;
        victim_pair = Some((victim, victim_ship));
        let (guardian, guardian_ship) = f.add_pilot().await;
        let hull = f.sde.types.get(f.world.ball(f.system, attacker_ship).await.ok_or_else(|| anyhow!("attacker ship ball"))?.type_id)
            .ok_or_else(|| anyhow!("attacker hull type"))?;
        let (module_type, effect) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.attribute(ATTR_CPU).zip(hull.fit_attribute(ATTR_CPU_OUTPUT)).is_some_and(|(need, out)| out >= need)
            && t.attribute(ATTR_POWER).zip(hull.fit_attribute(ATTR_POWER_OUTPUT)).is_some_and(|(need, out)| out >= need))
            .find_map(|t| t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id))
                .find(|e| e.kind == EffectKind::Ecm && e.has(EffectFlag::Offensive)).map(|e| (t.type_id, e)))
            .ok_or_else(|| anyhow!("published offensive ECM module unavailable"))?;
        let drone_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
            && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
            .ok_or_else(|| anyhow!("published combat drone unavailable"))?.type_id;
        let module: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&module_type, &attacker, &attacker_ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
        module_id = Some(module);
        f.set(attacker_ship, ATTR_MAX_TARGET_RANGE, 100_000.0).await;
        f.set(attacker_ship, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(attacker_ship, ATTR_MAX_LOCKED_TARGETS, 10.0).await;
        f.set(victim_ship, SENSORS[0], 100.0).await;
        if let Some(attr) = effect.attribute(EffectAttribute::Range) { f.set(module, attr, 100_000.0).await; }
        for attr in JAM_STRENGTH { f.set(module, attr, 0.0).await; }
        f.set(module, effect.attribute(EffectAttribute::Duration).expect("ECM duration"), 2000.0).await;
        f.owner = guardian; f.ship = guardian_ship;
        f.client.session.apply(vec![("charid", Value::Int(guardian)), ("shipid", Value::Int(guardian_ship))]);
        form_fleet(&mut f, victim, victim_ship).await?;
        f.train_drone(drone_type).await;
        f.set(guardian_ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        let guard = f.drone(drone_type, 1, true).await;
        f.set(guard, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(guard, ATTR_MAX_RANGE, 100_000.0).await;
        let args = [Value::list([Value::tuple([Value::Int(guard), Value::Int(1)])]), Value::None, Value::Bool(false)];
        ensure!(f.call("ship", "LaunchDrones", &args).await.map_err(|_| anyhow!("guard launch refused"))? == Value::Dict(vec![(Value::Int(guard), Value::list([Value::Int(guard)]))]), "guard launch failed");
        let args = [Value::Int(victim), Value::list([Value::Int(guard)])];
        ensure!(f.call("entity", "CmdGuard", &args).await.map_err(|_| anyhow!("Guard refused"))? == Value::Dict(vec![]), "Guard command failed");
        f.owner = victim; f.ship = victim_ship;
        f.client.session.apply(vec![("charid", Value::Int(victim)), ("shipid", Value::Int(victim_ship))]);
        f.train_drone(drone_type).await;
        f.set(victim_ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        let automatic = f.drone(drone_type, 1, true).await;
        f.set(automatic, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(automatic, ATTR_MAX_RANGE, 100_000.0).await;
        let args = [Value::list([Value::tuple([Value::Int(automatic), Value::Int(1)])]), Value::None, Value::Bool(false)];
        ensure!(f.call("ship", "LaunchDrones", &args).await.map_err(|_| anyhow!("automatic launch refused"))? == Value::Dict(vec![(Value::Int(automatic), Value::list([Value::Int(automatic)]))]), "automatic launch failed");
        settings::apply(victim, settings::DroneSettings::default(), settings::Patch { aggressive: Some(true), focus_fire: Some(true) });
        f.owner = attacker; f.ship = attacker_ship;
        f.client.session.apply(vec![("charid", Value::Int(attacker)), ("shipid", Value::Int(attacker_ship))]);
        let cx = combat::ctx(&f.client).ok_or_else(|| anyhow!("missing combat context"))?;
        targeting_charges::add_target_ctx(&cx, victim_ship, false).await.map_err(|_| anyhow!("lock refused"))?;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while !targeting_charges::targets_of(attacker).contains(&victim_ship) {
                ensure!(f.world.after(f.system, 100).await, "actor stopped during lock");
            }
            Ok::<_, anyhow::Error>(())
        }).await.map_err(|_| anyhow!("lock timed out"))??;
        let fit = Fit::load_in_space(&f.db, &f.world, f.system, attacker, attacker_ship).await.map_err(|_| anyhow!("source fit"))?;
        let victim_fit = Fit::load_in_space(&f.db, &f.world, f.system, victim, victim_ship).await.map_err(|_| anyhow!("victim fit"))?;
        ensure!(primary(SENSORS.map(|a| victim_fit.value(Key::Ship, a))).is_some(), "victim has no positive sensor");
        ensure!(JAM_STRENGTH.into_iter().all(|a| fit.value(Key::Module(module), a) == 0.0), "ECM strength was not zero");
        for (ship, fit) in [(attacker_ship, fit), (victim_ship, victim_fit)] {
            let condition = crate::services::combat::prepare_condition_with_fit(&f.db, ship, &fit).await.map_err(|_| anyhow!("condition load"))?;
            f.world.with(f.system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.ok_or_else(|| anyhow!("actor stopped"))?;
        }
        let (notices, mut received) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(attacker, notices);
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module, ActiveModule { char_id: attacker, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: Some(victim_ship), repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module, generation: generation.clone(), armed: false,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id: module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
            source_membership: None, target_type: None,
            inputs: crate::services::effects::parameters::CycleInputs::default() };
        let reuse = std::mem::take(&mut cycle.inputs);
        let mut cycle = f.world.with(f.system, move |s| {
            let mut cycle = cycle;
            cycle.inputs = s.with_combat_fit(attacker_ship, |fit, _, stamp| {
                cycle.source_membership = Some(stamp.membership);
                crate::services::effects::parameters::capture_cycle(fit, &cycle, stamp, reuse)
            }).expect("resident attacker fit").expect("cycle inputs");
            cycle
        }).await.ok_or_else(|| anyhow!("actor stopped"))?;
        ensure!(!crimewatch::retaliation_allowed(victim, attacker, crate::client::filetime_now()), "fixture starts with engagement");
        {
            let mut future = std::pin::pin!(start(&cx, &mut cycle));
            let first = std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await;
            ensure!(first.is_pending(), "candidate snapshot did not yield");
            ensure!(f.world.with(f.system, |_| ()).await.is_some(), "actor stopped after candidate snapshot");
            let second = std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await;
            ensure!(second.is_pending(), "second actor snapshot did not yield");
            let stopped = generation.clone();
            ensure!(f.world.with(f.system, move |_| stopped.store(true, Ordering::Relaxed)).await.is_some(), "actor stopped before invalidation");
            future.await.map_err(|_| anyhow!("stale cycle failed"))?;
        }
        ensure!(!combat::jamming::is_jammed(victim_ship, crate::space::now_ms()), "stale attempt jammed victim");
        ensure!(!crimewatch::retaliation_allowed(victim, attacker, crate::client::filetime_now()) && crimewatch::weapons_timer(attacker).is_none(), "stale attempt created Crimewatch state");
        while let Ok((name, _)) = received.try_recv() {
            ensure!(!matches!(name.as_str(), "OnWeaponsTimerUpdate" | "OnPvpTimerUpdate" | "OnCriminalTimerUpdate" | "OnSystemCriminalFlagUpdates" | "OnCrimewatchEngagementCreated"), "stale attempt emitted {name}");
        }
        ensure!(f.world.after(f.system, 1000).await, "actor stopped");
        ensure!(f.world.with(f.system, move |s| s.drone(guard).is_some_and(|d| d.target.is_none()) && s.drone(automatic).is_some_and(|d| d.order == DroneOrder::Idle)).await == Some(true), "stale attempt triggered Guard or automatic acquisition");
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module, ActiveModule { char_id: attacker, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module, generation.clone());
        }
        cycle.stop = generation.clone();
        cycle.owner = ActivationOwner { master: module, generation, armed: false,
            combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id: module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
            pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id };
        start(&cx, &mut cycle).await.map_err(|_| anyhow!("admitted cycle failed"))?;
        ensure!(!combat::jamming::is_jammed(victim_ship, crate::space::now_ms()), "zero-strength attempt jammed victim");
        ensure!(crimewatch::retaliation_allowed(victim, attacker, crate::client::filetime_now()), "admitted attempt did not create engagement");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if f.world.with(f.system, move |s| s.drone(guard).is_some_and(|d| d.target == Some(attacker_ship)) && s.drone(automatic).is_some_and(|d| d.target == Some(attacker_ship))).await == Some(true) { return Ok::<_, anyhow::Error>(()); }
                ensure!(f.world.after(f.system, 100).await, "actor stopped waiting for Guard and automatic acquisition");
            }
        }).await.map_err(|_| anyhow!("admitted attack did not reach Guard and incoming roster"))??;
        let required = crate::services::skills::requirements::requirements(&f.sde, module_type).collect::<Vec<_>>();
        crate::services::skills::gm::apply(&mut f.client, attacker, crate::services::skills::gm::Change::Set(required)).await.map_err(|_| anyhow!("module skill seed refused"))?;
        crate::services::effects::tests::fixture::gm_value(&mut f.client, victim_ship, SENSORS[0], 0.0).await;
        ensure!(crate::dao::items::dogma::online(&f.db, attacker_ship).await?.iter().any(|entry| entry.item_id == module), "fixture pilot must meet the module's prerequisites after first GM write (module {})", module);
        for attribute in SENSORS.into_iter().skip(1) {
            crate::services::effects::tests::fixture::gm_value(&mut f.client, victim_ship, attribute, 0.0).await;
        }
        for attribute in JAM_STRENGTH {
            crate::services::effects::tests::fixture::gm_value(&mut f.client, module, attribute, 1e9).await;
        }
        cycle = crate::services::effects::tests::fixture::capture_cycle(cycle).await;
        let zero_defense = start(&cx, &mut cycle).await.map_err(|_| anyhow!("zero-defense admission"))?;
        ensure!(matches!(zero_defense, CycleOutcome::Continue), "zero-defense start not admitted: {zero_defense:?}");
        ensure!(!combat::jamming::is_jammed(victim_ship, crate::space::now_ms()), "zero defense jammed");
        for attribute in SENSORS {
            crate::services::effects::tests::fixture::gm_value(&mut f.client, victim_ship, attribute, 100.0).await;
        }
        let current_defense = start(&cx, &mut cycle).await.map_err(|_| anyhow!("current-defense admission"))?;
        ensure!(matches!(current_defense, CycleOutcome::Continue), "current-defense start not admitted: {current_defense:?}");
        ensure!(combat::jamming::is_jammed(victim_ship, crate::space::now_ms()), "current defense was not consumed");
        f.world.with(f.system, move |s| combat::jamming::clear(s, victim_ship)).await.unwrap();
        cycle = crate::services::effects::tests::fixture::capture_cycle(cycle).await;
        let (capacity, tau) = cycle.inputs.frame.as_ref().unwrap().capacitor;
        crate::services::combat::groups_health::fill_capacitor(attacker_ship, capacity, tau, crate::client::filetime_now());
        cycle.repeat = 2;
        cycle.cycles = 0;
        cycle.owner.armed = true;
        while received.try_recv().is_ok() {}
        f.world.with(f.system, move |s| {
            let now = s.now_ms();
            crate::services::effects::lifecycle::cycle_start(s, cycle, now);
        }).await.unwrap();
        let mut applied = 0;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while applied < 2 {
                let (name, args) = received.recv().await.expect("ECM notices");
                if name == "OnJamStart" && args.get(1) == Some(&Value::Int(module))
                    && args.get(2) == Some(&Value::Int(victim_ship)) { applied += 1; }
            }
            while combat::state().active.contains_key(&module) { assert!(f.world.after(f.system, 100).await); }
        }).await.expect("two scheduled ECM applications");
        assert_eq!(applied, 2);
        Ok(())
    }).await;
    if let Some(module) = module_id {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module);
    }
    if let Some((victim, ship)) = victim_pair {
        settings::logged_off(victim);
        targeting_charges::clear_locks_on(&f.world, f.system, ship, victim);
    }
    targeting_charges::clear_locks_on(&f.world, f.system, attacker_ship, attacker);
    f.owner = attacker; f.ship = attacker_ship;
    f.client.session.apply(vec![("charid", Value::Int(attacker)), ("shipid", Value::Int(attacker_ship))]);
    f.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn ecm_final_consumer_expires_defense_and_rejects_replaced_body() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && SENSORS.iter().any(|&a| ty.attribute(a).is_some_and(|v| v > 0.0))).unwrap().type_id;
    let source = -986_201;
    let target = -986_202;
    let fit = Fit::for_space_type(sde.clone(), hull, vec![]);
    let (fit, active, expired) = crate::services::effects::tests::fixture::due_fit(
        fit, &sde, |fit| primary(SENSORS.map(|a| fit.value(Key::Ship, a))).unwrap().1);
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
    let mut system = System::default();
    system.insert(crate::services::effects::tests::fixture::ball(source, hull, 1, None, 0.0));
    let victim = crate::services::effects::tests::fixture::ball(target, hull, 2, None, 0.0);
    system.add_persistent_ship(victim.clone());
    system.install_condition(target, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: 0 }, fit);
    let membership = system.combat_membership_generation(target);
    let strength = active.min(expired) / 2.0;
    let roll = (chance(strength, active, 1.0) + chance(strength, expired, 1.0)) / 2.0;
    assert_ne!(roll < chance(strength, active, 1.0), roll < chance(strength, expired, 1.0));
    let src = Source { id: source, type_id: Some(hull), module: -986_203,
        optimal: 1000.0, falloff: 0.0, strengths: [strength; 4], duration: 1000.0,
        burst: false, offensive: false, revision: None, owner_revision: None };
    assert_eq!(system.combat_fit_revision(target), Some(0));
    let gate = TargetGate { victim: victim.clone(), membership, owner: None,
        player: None, guard: None, roll };
    assert!(apply_one_in_actor(&mut system, &sde, &src, gate).is_some());
    assert_eq!(combat::jamming::is_jammed(target, system.now_ms()),
        roll < chance(strength, expired, 1.0));
    assert!(system.combat_fit_revision(target).unwrap() > 0);
    combat::jamming::clear(&mut system, target);
    system.remove_ball(target);
    system.add_persistent_ship(victim.clone());
    let fit = Fit::for_space_type(sde.clone(), hull, vec![]);
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(target, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: 0 }, fit);
    assert_ne!(system.combat_membership_generation(target), membership);
    let gate = TargetGate { victim, membership, owner: None, player: None, guard: None, roll: 0.0 };
    assert!(apply_one_in_actor(&mut system, &sde, &src, gate).is_none());
    assert!(!combat::jamming::is_jammed(target, system.now_ms()));
    combat::forget_ship(source);
    combat::forget_ship(target);
}
