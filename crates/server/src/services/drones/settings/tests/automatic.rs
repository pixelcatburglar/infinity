use std::{future::Future, task::Poll, time::Duration};
use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use infinity_core::{
    consts::{*, dogma::*},
};
use crate::{
    services::{Call, combat::ATTR_DAMAGE, drones::tests::fixture::Fixture,
    effects::{propulsion, registry, EffectKind}, fit::{Fit, Key}},
    space::drone::DroneOrder,
};

async fn catch_scenario(future: impl Future<Output = Result<()>>) -> std::thread::Result<Result<()>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
        Ok(Poll::Pending) => Poll::Pending,
        Err(panic) => Poll::Ready(Err(panic)),
    }).await
}

async fn settings(f: &mut Fixture, aggressive: bool) -> Result<()> {
    let args = [Value::Dict(vec![(Value::Int(1275), Value::Bool(aggressive)),
        (Value::Int(1297), Value::Bool(true))])];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "ChangeDroneSettings", args: &args,
        kwargs: &kwargs, bound: true, bind_params: None };
    let result = crate::services::dogma::dispatch(&mut f.client, &call).await
        .map_err(|e| anyhow!("settings dispatch: {e:?}"))?;
    ensure!(result == Value::None, "settings response was not None");
    ensure!(super::super::current(f.owner) == (super::super::DroneSettings { aggressive, focus_fire: true }),
        "settings were not synchronized");
    Ok(())
}

async fn incoming(f: &Fixture, rx: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>, npc: i64) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            while let Ok((event, args)) = rx.try_recv() {
                if event == "OnDamageMessage" && args.first().is_some_and(|v| matches!(v,
                    Value::Dict(fields) if fields.iter().any(|(k, v)| *k == Value::str("source") && *v == Value::Int(npc)))) {
                    return Ok(());
                }
            }
            ensure!(f.world.after(f.system, 100).await, "actor stopped awaiting NPC attack");
        }
    }).await.map_err(|_| anyhow!("NPC attack timed out"))?
}

async fn spawn(f: &Fixture, npc_type: i32) -> Result<i64> {
    crate::services::combat::npc::spawn(&f.db, &f.world, f.system, f.sde.types.get(npc_type).unwrap(),
        1, [10.0, 0.0, 0.0], crate::space::now_ms(), Some(f.owner)).await
        .map_err(|e| anyhow!("NPC spawn: {e:?}"))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn synchronized_aggression_controls_real_npc_retaliation() -> Result<()> {
    let mut f = Fixture::new().await;
    let result = catch_scenario(async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0 && v <= 5.0)
            && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
            .ok_or_else(|| anyhow!("no low-bandwidth turret drone"))?.type_id;
        let npc_type = f.sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_ENTITY)
            && ATTR_DAMAGE.iter().any(|a| t.attribute(*a).is_some_and(|v| v > 0.0))
            && t.attribute(ATTR_DAMAGE_MULTIPLIER).unwrap_or(1.0) > 0.0
            && t.attribute(ATTR_SCAN_RESOLUTION).is_some_and(|v| v > 0.0))
            .max_by(|a, b| a.attribute(ATTR_SCAN_RESOLUTION).unwrap().total_cmp(&b.attribute(ATTR_SCAN_RESOLUTION).unwrap()))
            .ok_or_else(|| anyhow!("no turret-capable NPC"))?.type_id;
        f.train_drone(ty).await;
        let drone = f.drone(ty, 1, true).await;
        for (item, attr, value) in [(f.ship, ATTR_DRONE_BANDWIDTH, 100.0),
            (f.ship, ATTR_SIGNATURE_RADIUS, 1000.0), (f.ship, ATTR_SHIELD_CAPACITY, 1e12),
            (f.ship, ATTR_ARMOR_HP, 1e12), (f.ship, ATTR_STRUCTURE_HP, 1e12),
            (drone, ATTR_MAX_RANGE, 100_000.0), (drone, ATTR_MAX_TARGET_RANGE, 100_000.0),
            (drone, ATTR_EM_DAMAGE, 0.0), (drone, ATTR_THERMAL_DAMAGE, 0.0),
            (drone, ATTR_KINETIC_DAMAGE, 0.0), (drone, ATTR_EXPLOSIVE_DAMAGE, 0.0)] {
            f.set(item, attr, value).await;
        }
        let fit = Fit::load_in_space(&f.db, &f.world, f.system, f.owner, f.ship).await
            .map_err(|e| anyhow!("owner fit: {e:?}"))?;
        let motion = propulsion::kinematics(&fit, Key::Ship);
        let ship = f.ship;
        f.world.with(f.system, move |s| propulsion::queue_kinematics(s, ship, motion)).await
            .ok_or_else(|| anyhow!("actor stopped queuing owner kinematics"))?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if f.world.ball(f.system, ship).await.and_then(|b| b.signature_radius) == Some(1000.0) { return Ok::<(), anyhow::Error>(()); }
                ensure!(f.world.after(f.system, 100).await, "actor stopped applying owner kinematics");
            }
        }).await.map_err(|_| anyhow!("owner has no effective signature"))??;
        settings(&mut f, false).await?;
        let args = [Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
        let launched = f.call("ship", "LaunchDrones", &args).await.map_err(|e| anyhow!("launch: {e:?}"))?;
        ensure!(matches!(&launched, Value::Dict(rows) if rows.iter().any(|(_, v)| *v == Value::list([Value::Int(drone)]))), "drone launch failed: {launched:?}");
        f.world.with(f.system, move |s| s.set_ball_position(drone, [1000.0, 0.0, 0.0], |_, _| true)).await
            .ok_or_else(|| anyhow!("actor stopped positioning drone"))?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if f.world.ball(f.system, drone).await.is_some_and(|b| b.pos == [1000.0, 0.0, 0.0]) { return Ok::<(), anyhow::Error>(()); }
                ensure!(f.world.after(f.system, 100).await, "actor stopped positioning drone");
            }
        }).await.map_err(|_| anyhow!("drone position was not applied"))??;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx);
        let first = spawn(&f, npc_type).await?;
        incoming(&f, &mut rx, first).await?;
        ensure!(f.world.after(f.system, 500).await, "actor stopped after passive attack");
        ensure!(f.world.with(f.system, move |s| s.drone(drone).map(|d| d.order)).await.flatten() == Some(DroneOrder::Idle),
            "passive drone reacted to incoming attack");
        f.world.remove_ball(f.system, first).await;
        crate::services::combat::forget_gm_spawn(&f.world, first);
        crate::services::combat::forget_ship(first);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if f.world.ball(f.system, first).await.is_none() { return Ok::<(), anyhow::Error>(()); }
                ensure!(f.world.after(f.system, 100).await, "actor stopped removing first aggressor");
            }
        }).await.map_err(|_| anyhow!("first aggressor was not removed"))??;
        settings(&mut f, true).await?;
        let second = spawn(&f, npc_type).await?;
        incoming(&f, &mut rx, second).await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if f.world.with(f.system, move |s| s.drone(drone).map(|d| d.order)).await.flatten() == Some(DroneOrder::Engage(second)) { return Ok(()); }
                ensure!(f.world.after(f.system, 100).await, "actor stopped awaiting retaliation");
            }
        }).await.map_err(|_| anyhow!("aggressive drone did not engage actual aggressor"))??;
        Ok(())
    }).await;
    super::super::logged_off(f.owner);
    f.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
