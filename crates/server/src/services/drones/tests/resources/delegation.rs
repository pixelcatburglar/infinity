use std::future::Future;
use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::{Call, effects::{registry, EffectKind, JammingType, registry::{EffectAttribute, EffectFlag}}}, space::drone::DroneOrder};
use super::{fleet::form_fleet, super::fixture::Fixture};

fn call_error(error: crate::services::CallError) -> anyhow::Error {
    match error {
        crate::services::CallError::Internal(error) => error,
        crate::services::CallError::User { msg, .. } => anyhow!("{msg}"),
    }
}

async fn catch_scenario<F: Future<Output = Result<()>>>(future: F) -> std::thread::Result<Result<()>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(std::task::Poll::Ready(result)) => std::task::Poll::Ready(Ok(result)),
        Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
        Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await
}

async fn lock(f: &mut Fixture, owner: i64, target: i64) -> Result<()> {
    let args = [Value::Int(target)];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "AddTarget", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dogma::dispatch(&mut f.client, &call).await.map_err(call_error)?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !crate::services::combat::targeting_charges::targets_of(owner).contains(&target) {
            ensure!(f.world.after(f.system, 100).await, "system stopped during lock");
        }
        Ok::<_, anyhow::Error>(())
    }).await.map_err(|_| anyhow!("target lock timed out"))??;
    Ok(())
}

async fn launch(f: &mut Fixture, drone: i64) -> Result<()> {
    let args = [Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let result = f.call("ship", "LaunchDrones", &args).await.map_err(call_error)?;
    ensure!(result == Value::Dict(vec![(Value::Int(drone), Value::list([Value::Int(drone)]))]), "launch refused: {result:?}");
    Ok(())
}

async fn command(f: &mut Fixture, name: &str, args: &[Value]) -> Result<()> {
    let result = f.call("entity", name, args).await.map_err(call_error)?;
    ensure!(result == Value::Dict(vec![]), "{name} refused: {result:?}");
    Ok(())
}

async fn delegated_target(f: &Fixture, drone: i64, order: DroneOrder, target: i64) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let state = f.world.with(f.system, move |s| s.drone(drone)).await.ok_or_else(|| anyhow!("system stopped"))?;
            if state.is_some_and(|d| d.order == order && d.target == Some(target)) { return Ok::<_, anyhow::Error>(()); }
            ensure!(f.world.after(f.system, 100).await, "system stopped waiting for delegated target");
        }
    }).await.map_err(|_| anyhow!("delegated target timed out"))?
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn fleet_assist_and_guard_follow_admitted_nonweapon_attacks() -> Result<()> {
    let mut f = Fixture::new().await;
    let primary = (f.owner, f.ship);
    let mut secondary = Vec::new();
    let result = catch_scenario(async {
        let (delegate, delegate_ship) = f.add_pilot().await;
        secondary.push((delegate, delegate_ship));
        let (attacker, attacker_ship) = f.add_pilot().await;
        secondary.push((attacker, attacker_ship));
        let fleet = form_fleet(&mut f, delegate, delegate_ship).await?;
        ensure!(!crate::services::fleets::same_fleet(primary.0, attacker), "attacker unexpectedly in fleet");
        let (combat_type, combat_effect) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0))
            .find_map(|t| t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id))
                .find(|e| e.kind == EffectKind::Turret).map(|e| (t.type_id, e)))
            .ok_or_else(|| anyhow!("no published combat drone"))?;
        let (ecm_type, ecm_effect) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
            && !t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| matches!(e.kind, EffectKind::Turret | EffectKind::Missile))))
            .find_map(|t| t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id))
                .find(|e| e.kind == EffectKind::Ecm && e.has(EffectFlag::Offensive)).map(|e| (t.type_id, e)))
            .ok_or_else(|| anyhow!("no published offensive ECM drone"))?;
        let (web_type, web_effect, web_name) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE))
            .find_map(|t| t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id))
                .find(|e| e.kind == (EffectKind::Projected { jamming: JammingType::Webify }) && e.has(EffectFlag::Offensive))
                .and_then(|e| f.sde.dogma.effect(e.id as i32).and_then(|row| row.name.clone()).map(|name| (t.type_id, e, name))))
            .ok_or_else(|| anyhow!("no published offensive webifier"))?;
        f.train_drone(combat_type).await;
        let drone = f.drone(combat_type, 1, true).await;
        f.set(primary.1, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(drone, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(drone, ATTR_MAX_RANGE, 100_000.0).await;
        f.set(drone, combat_effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE), 500.0).await;
        launch(&mut f, drone).await?;
        command(&mut f, "CmdAssist", &[Value::Int(delegate), Value::list([Value::Int(drone)])]).await?;
        let module: i64 = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&web_type, &delegate, &delegate_ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
        f.set(delegate_ship, ATTR_MAX_TARGET_RANGE, 100_000.0).await;
        f.set(delegate_ship, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        if let Some(attr) = web_effect.attribute(EffectAttribute::Range) { f.set(module, attr, 100_000.0).await; }
        let npc_type = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY) && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0))
            .ok_or_else(|| anyhow!("no NPC target"))?.type_id;
        let npc = crate::services::combat::npc::spawn(&f.db, &f.world, f.system, f.sde.types.get(npc_type).unwrap(), 1, [1000.0, 0.0, 0.0], crate::space::now_ms(), None).await.map_err(call_error)?;
        f.client.session.apply(vec![("charid", Value::Int(delegate)), ("shipid", Value::Int(delegate_ship))]);
        let args = [Value::Int(module), Value::str(&web_name), Value::Int(npc), Value::Int(0)];
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "dogmaIM", method: "Activate", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        ensure!(crate::services::dogma::dispatch(&mut f.client, &call).await.is_err(), "unlocked web unexpectedly accepted");
        let current = f.world.with(f.system, move |s| s.drone(drone)).await.flatten().ok_or_else(|| anyhow!("missing drone"))?;
        ensure!(current.order == DroneOrder::Assist(delegate) && current.target.is_none(), "refused web retargeted assist");
        lock(&mut f, delegate, npc).await?;
        crate::services::dogma::dispatch(&mut f.client, &call).await.map_err(call_error)?;
        delegated_target(&f, drone, DroneOrder::Assist(delegate), npc).await?;
        crate::services::combat::reset(&f.client);
        f.client.session.apply(vec![("charid", Value::Int(primary.0)), ("shipid", Value::Int(primary.1))]);
        command(&mut f, "CmdGuard", &[Value::Int(delegate), Value::list([Value::Int(drone)])]).await?;
        f.owner = attacker; f.ship = attacker_ship;
        f.train_drone(ecm_type).await;
        let jammer = f.drone(ecm_type, 1, true).await;
        f.owner = primary.0; f.ship = primary.1;
        f.set(attacker_ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(attacker_ship, ATTR_MAX_TARGET_RANGE, 100_000.0).await;
        f.set(attacker_ship, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(jammer, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(jammer, ATTR_MAX_RANGE, 100_000.0).await;
        for attr in [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP] { f.set(jammer, attr, 1_000_000_000.0).await; }
        if let Some(attr) = ecm_effect.attribute(EffectAttribute::Range) { f.set(jammer, attr, 100_000.0).await; }
        for name in ["scanGravimetricStrengthBonus", "scanLadarStrengthBonus", "scanMagnetometricStrengthBonus", "scanRadarStrengthBonus"] {
            let attr = f.sde.dogma.attributes_by_name(name).next().ok_or_else(|| anyhow!("missing ECM attribute {name}"))?.attribute_id;
            f.set(jammer, attr, 0.0).await;
        }
        f.client.session.apply(vec![("charid", Value::Int(attacker)), ("shipid", Value::Int(attacker_ship))]);
        launch(&mut f, jammer).await?;
        lock(&mut f, attacker, delegate_ship).await?;
        command(&mut f, "CmdEngage", &[Value::list([Value::Int(jammer)]), Value::Int(delegate_ship)]).await?;
        delegated_target(&f, drone, DroneOrder::Guard(delegate), jammer).await?;
        ensure!(!crate::services::combat::jamming::is_jammed(delegate_ship, crate::space::now_ms()), "zero-strength ECM jammed delegate");
        crate::services::combat::reset(&f.client);
        f.client.session.apply(vec![("charid", Value::Int(delegate)), ("shipid", Value::Int(delegate_ship))]);
        let params = Value::Int(fleet); let kwargs = Value::None;
        let leave = Call { service: "fleetObjectHandler", method: "LeaveFleet", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&params) };
        crate::services::fleets::dispatch(&mut f.client, &leave).await.ok_or_else(|| anyhow!("missing LeaveFleet"))?.map_err(call_error)?;
        ensure!(!crate::services::fleets::same_fleet(primary.0, delegate), "delegate remained in fleet");
        let health = f.world.ball(f.system, jammer).await.ok_or_else(|| anyhow!("ECM drone missing after fleet departure"))?.health;
        f.client.session.apply(vec![("charid", Value::Int(primary.0)), ("shipid", Value::Int(primary.1))]);
        ensure!(f.world.after(f.system, 2000).await, "system stopped after fleet departure");
        let state = f.world.with(f.system, move |s| s.drone(drone)).await.flatten().ok_or_else(|| anyhow!("missing guard drone"))?;
        let after = f.world.ball(f.system, jammer).await.ok_or_else(|| anyhow!("ECM drone missing after stale guard cycle"))?.health;
        ensure!(after == health, "guard damaged ECM drone after fleet departure");
        ensure!(state.order != DroneOrder::Guard(delegate) || state.target != Some(jammer), "guard remained active after fleet departure");
        Ok(())
    }).await;
    for (owner, ship) in secondary {
        f.client.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(ship))]);
        crate::services::combat::reset(&f.client);
    }
    f.owner = primary.0; f.ship = primary.1;
    f.client.session.apply(vec![("charid", Value::Int(primary.0)), ("shipid", Value::Int(primary.1))]);
    f.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
