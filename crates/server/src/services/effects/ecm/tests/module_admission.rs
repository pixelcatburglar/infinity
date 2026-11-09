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
        let (module_type, effect) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE))
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
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        ensure!(!crimewatch::retaliation_allowed(victim, attacker, crate::client::filetime_now()), "fixture starts with engagement");
        {
            let mut future = std::pin::pin!(start(&cx, &fit, &mut cycle));
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
        start(&cx, &fit, &mut cycle).await.map_err(|_| anyhow!("admitted cycle failed"))?;
        ensure!(!combat::jamming::is_jammed(victim_ship, crate::space::now_ms()), "zero-strength attempt jammed victim");
        ensure!(crimewatch::retaliation_allowed(victim, attacker, crate::client::filetime_now()), "admitted attempt did not create engagement");
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if f.world.with(f.system, move |s| s.drone(guard).is_some_and(|d| d.target == Some(attacker_ship)) && s.drone(automatic).is_some_and(|d| d.target == Some(attacker_ship))).await == Some(true) { return Ok::<_, anyhow::Error>(()); }
                ensure!(f.world.after(f.system, 100).await, "actor stopped waiting for Guard and automatic acquisition");
            }
        }).await.map_err(|_| anyhow!("admitted attack did not reach Guard and incoming roster"))??;
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
