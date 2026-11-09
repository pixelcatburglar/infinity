use std::{future::Future, task::Poll, time::Duration};
use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::{combat::ATTR_DAMAGE, effects::{registry, EffectKind, space_source::SourceGuard, weapons::{self, Missile}}, fit::{Fit, Key}}, space::drone::DroneOrder};
use super::fixture::Fixture;

mod missile_persistent;

async fn catch_scenario(future: impl Future<Output = Result<()>>) -> std::thread::Result<Result<()>> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
        Ok(Poll::Pending) => Poll::Pending,
        Err(panic) => Poll::Ready(Err(panic)),
    }).await
}

fn drone_type(f: &Fixture) -> Result<i32> {
    f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
        && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
        && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
        .map(|t| t.type_id).ok_or_else(|| anyhow!("published turret drone unavailable"))
}

async fn launch(f: &mut Fixture, drone: i64) -> Result<()> {
    let args = [Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let Value::Dict(rows) = f.call("ship", "LaunchDrones", &args).await.ok().ok_or_else(|| anyhow!("launch refused"))? else { return Err(anyhow!("launch mapping")) };
    ensure!(rows == vec![(Value::Int(drone), Value::list([Value::Int(drone)]))], "launch failed: {rows:?}");
    Ok(())
}

async fn pair(f: &mut Fixture) -> Result<(i64, i64, i64, i64)> {
    let ty = drone_type(f)?;
    let (owner, ship) = (f.owner, f.ship);
    let (other, other_ship) = f.add_pilot().await;
    f.train_drone(ty).await;
    let source = f.drone(ty, 1, true).await;
    for (item, attr, value) in [(ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0), (source, ATTR_RATE_OF_FIRE, 0.0),
        (source, ATTR_SCAN_RESOLUTION, 1_000_000.0), (source, ATTR_MAX_RANGE, 1_000_000.0),
        (source, ATTR_MAX_TARGET_RANGE, 1_000_000.0), (source, ATTR_EM_DAMAGE, 10.0),
        (source, ATTR_DAMAGE_MULTIPLIER, 1.0)] { f.set(item, attr, value).await; }
    launch(f, source).await?;
    f.owner = other; f.ship = other_ship;
    f.client.session.apply(vec![("charid", Value::Int(other)), ("shipid", Value::Int(other_ship))]);
    f.train_drone(ty).await;
    let victim = f.drone(ty, 1, true).await;
    for (item, attr, value) in [(other_ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
        (victim, ATTR_SHIELD_CAPACITY, 1_000_000.0), (victim, ATTR_SHIELD_EM_DAMAGE_RESONANCE, 1.0),
        (victim, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, 1.0), (victim, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, 1.0),
        (victim, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, 1.0),
        (victim, ATTR_SIGNATURE_RADIUS, 10_000.0)] { f.set(item, attr, value).await; }
    let launched = launch(f, victim).await;
    f.owner = owner; f.ship = ship;
    f.client.session.apply(vec![("charid", Value::Int(owner)), ("shipid", Value::Int(ship))]);
    launched?;
    // Keep the two physical drones apart and stationary for deterministic turret geometry.
    f.world.with(f.system, move |s| {
        for (id, x) in [(source, 100.0), (victim, 200.0)] {
            s.command(id, move |b, _, _| { b.pos = [x, 0.0, 0.0]; b.old_pos = b.pos; b.vel = [0.0; 3]; b.old_vel = b.vel; b.max_vel = 0.0; vec![] });
        }
    }).await.ok_or_else(|| anyhow!("actor stopped"))?;
    Ok((source, victim, other, other_ship))
}

async fn locked(f: &Fixture, source: i64, target: i64) -> Result<SourceGuard> {
    let (owner, ship) = (f.owner, f.ship);
    ensure!(f.world.with(f.system, move |s| s.drone_order(source, owner, ship, DroneOrder::Engage(target))).await == Some(true), "engage rejected");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(Some(guard)) = f.world.with(f.system, move |s| {
                let guard = SourceGuard::Drone { source, target, generation: s.drone(source)?.generation };
                guard.valid(s).then_some(guard)
            }).await { return Ok(guard); }
            ensure!(f.world.after(f.system, 100).await, "actor stopped waiting for lock");
        }
    }).await.map_err(|_| anyhow!("drone lock timed out"))?
}
async fn admitted(f: &Fixture, guard: SourceGuard, source: i64, weapon_type: i32) -> Result<crate::services::effects::space_source::PreparedCombatAttempt> {
    let weak = std::sync::Arc::downgrade(&f.world);
    let system = f.system;
    let facts = f.world.with(system, move |s| s.prepare_body_combat_source(source, None, None, Some(weapon_type),
        crate::lua::events::resources::FactOrigin::Natural, move |s| guard.valid(s))).await
        .ok_or_else(|| anyhow!("source actor unavailable"))?.map_err(|error| anyhow!("source admission: {error:?}"))?;
    Ok(crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system))
}


fn has_field(args: &[Value], key: &str, id: i64) -> bool {
    matches!(args.first(), Some(Value::Dict(fields)) if fields.iter().any(|(k, v)| *k == Value::str(key) && *v == Value::Int(id)))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn two_owners_receive_drone_combat_attribution() -> Result<()> {
    let mut f = Fixture::new().await;
    let result = catch_scenario(async {
        let (source, victim, other, other_ship) = pair(&mut f).await?;
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx_a); f.world.register_pilot(other, tx_b);
        let checkpoint_gate = f.world.lock_condition_snapshot(f.system).await?;
        let before = crate::dao::items::dogma::overrides(&f.db, victim).await?.iter()
            .filter(|r| [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE].contains(&r.attribute_id))
            .map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>();
        let guard = locked(&f, source, victim).await?;
        let (from, target) = f.world.with(f.system, move |s| Some((s.sampled_ball(source)?, s.sampled_ball(victim)?))).await.flatten().ok_or_else(|| anyhow!("missing drones"))?;
        let fit = Fit::drone_in_space(&f.db, &f.world, f.system, f.owner, source).await.ok().ok_or_else(|| anyhow!("source fit"))?;
        let key = Key::Drone(source);
        let raw = ATTR_DAMAGE.map(|a| fit.value(key, a) * fit.value(key, ATTR_DAMAGE_MULTIPLIER).max(0.0));
        ensure!(raw.iter().sum::<f64>() > 0.0, "fitted source damage must be positive");
        let prehit = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim)).await.flatten()
            .ok_or_else(|| anyhow!("missing victim condition"))?;
        let attempt = admitted(&f, guard, source, from.type_id).await?;
        weapons::turret_shot(&f.db, &f.world, f.system, &from, &target, Some(f.owner), from.type_id, false,
            (fit.value(key, ATTR_MAX_RANGE), fit.value(key, ATTR_FALLOFF), fit.value(key, ATTR_TRACKING_SPEED), fit.value(key, ATTR_OPTIMAL_SIG_RADIUS)), raw, Some(guard), attempt).await.ok().ok_or_else(|| anyhow!("turret shot failed"))?;
        ensure!(f.world.with(f.system, move |s| s.ball(victim).is_some_and(|b| b.char_id.is_none()) && s.pilot_ball(other).is_some_and(|b| b.id == other_ship)).await == Some(true), "victim entered pilot index");
        let (owner, ship) = (f.owner, f.ship);
        ensure!(f.world.with(f.system, move |s| s.drone_order(source, owner, ship, DroneOrder::ReturnHome)).await == Some(true), "recall failed");
        let hit = f.world.with(f.system, move |s| s.combat_condition_snapshot(victim)).await.flatten()
            .ok_or_else(|| anyhow!("missing victim condition after hit"))?;
        ensure!(hit.revision > prehit.revision && hit.revision != hit.saved_revision
            && hit.condition.now[0] > 0.0 && hit.condition.now[0] < prehit.condition.now[0],
            "actor shield condition not damaged");
        let before_flush = crate::dao::items::dogma::overrides(&f.db, victim).await?.iter()
            .filter(|r| [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE].contains(&r.attribute_id))
            .map(|r| (r.attribute_id, r.value, r.updated_at)).collect::<Vec<_>>();
        ensure!(before_flush == before, "per-hit damage changed durable condition before checkpoint");
        drop(checkpoint_gate);
        f.world.flush_positions().await?;
        let condition = crate::dao::items::dogma::overrides(&f.db, victim).await?;
        let shield = condition.iter().find(|r| r.attribute_id == ATTR_SHIELD_CHARGE).ok_or_else(|| anyhow!("shield condition not checkpointed"))?;
        let dt = shield.updated_at.saturating_sub(hit.condition.shield_at).max(0) as f64 / FILETIME_SECOND as f64;
        let expected = crate::services::combat::groups_health::recharge(hit.condition.now[0], hit.condition.stats.max[0], hit.condition.stats.shield_tau_ms, dt);
        ensure!(shield.value == expected && shield.value > 0.0 && shield.value < hit.condition.stats.max[0],
            "checkpoint did not persist actual shield condition: {} != {expected}", shield.value);
        ensure!(f.world.with(f.system, move |s| { let at = s.combat_fact_time(); s.capture_dirty_condition(victim, true, at).is_none() }).await == Some(true),
            "checkpoint did not acknowledge victim condition");
        let mut a_hit = false; let mut b_hit = false; let mut a_engagement = false; let mut b_engagement = false;
        while let Ok((event, args)) = rx_a.try_recv() {
            if event == "OnDamageMessage" { a_hit |= has_field(&args, "target", victim); }
            if event == "OnCrimewatchEngagementCreated" { a_engagement |= args.first() == Some(&Value::Int(other)); }
        }
        while let Ok((event, args)) = rx_b.try_recv() {
            if event == "OnDamageMessage" { b_hit |= has_field(&args, "source", source); }
            if event == "OnCrimewatchEngagementCreated" { b_engagement |= args.first() == Some(&Value::Int(f.owner)); }
        }
        ensure!(a_hit && b_hit && a_engagement && b_engagement, "missing physical hit or owner engagement notifications");
        let npc_type = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)
            && ATTR_DAMAGE.iter().any(|a| t.attribute(*a).is_some_and(|v| v > 0.0))
            && t.attribute(ATTR_SCAN_RESOLUTION).is_some_and(|v| v > 0.0)).ok_or_else(|| anyhow!("NPC weapon data unavailable"))?.type_id;
        let pos = f.world.ball(f.system, victim).await.ok_or_else(|| anyhow!("victim disappeared"))?.pos;
        let npc = crate::services::combat::npc::spawn(&f.db, &f.world, f.system, f.sde.types.get(npc_type).unwrap(), 1, pos, crate::space::now_ms(), Some(other)).await.ok().ok_or_else(|| anyhow!("NPC spawn failed"))?;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                while let Ok((event, args)) = rx_b.try_recv() {
                    if event == "OnDamageMessage" && has_field(&args, "source", npc) { return Ok(()); }
                }
                ensure!(f.world.after(f.system, 100).await, "actor stopped awaiting NPC shot");
            }
        }).await.map_err(|_| anyhow!("NPC shot timed out"))??;
        Ok(())
    }).await;
    f.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn missile_generation_is_validated_at_insertion() -> Result<()> {
    let mut f = Fixture::new().await;
    let result = catch_scenario(async {
        let (source, victim, other, _) = pair(&mut f).await?;
        let charge = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_CHARGE)
            && t.attribute(ATTR_AOE_CLOUD_SIZE).is_some_and(|v| v > 0.0)
            && t.attribute(ATTR_AOE_VELOCITY).is_some_and(|v| v > 0.0)
            && t.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).is_some_and(|v| v > 0.0)
            && t.attribute(ATTR_MAX_VELOCITY).is_some_and(|v| v > 0.0)
            && t.attribute(ATTR_EXPLOSION_DELAY).is_some_and(|v| v > 0.0)
            && ATTR_DAMAGE.iter().any(|a| t.attribute(*a).is_some_and(|v| v > 0.0))).ok_or_else(|| anyhow!("published missile charge unavailable"))?;
        let missile = || Missile { charge: charge.type_id, raw: ATTR_DAMAGE.map(|a| charge.attribute(a).unwrap_or(0.0)),
            aoe_cloud: charge.attribute(ATTR_AOE_CLOUD_SIZE).unwrap(), aoe_velocity: charge.attribute(ATTR_AOE_VELOCITY).unwrap(),
            drf: charge.attribute(ATTR_AOE_DAMAGE_REDUCTION_FACTOR).unwrap(), velocity: charge.attribute(ATTR_MAX_VELOCITY).unwrap(),
            flight_range: charge.attribute(ATTR_MAX_VELOCITY).unwrap() * crate::space::ms_to_seconds(charge.attribute(ATTR_EXPLOSION_DELAY).unwrap()),
            weapon: charge.type_id, illegal: false };
        let guard = locked(&f, source, victim).await?;
        let (from, target) = f.world.with(f.system, move |s| Some((s.sampled_ball(source)?, s.sampled_ball(victim)?))).await.flatten().ok_or_else(|| anyhow!("missing drones"))?;
        let (tx_victim, mut rx_victim) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(other, tx_victim);
        let (tx_owner, mut rx_owner) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx_owner);
        let security_before = crate::services::crimewatch::security_status(&f.db, f.owner).await.ok().ok_or_else(|| anyhow!("security lookup failed"))?;
        let weapons_before = crate::services::crimewatch::weapons_timer(f.owner);
        let retaliation_before = crate::services::crimewatch::retaliation_allowed(other, f.owner, crate::client::filetime_now());
        let reverse_before = crate::services::crimewatch::retaliation_allowed(f.owner, other, crate::client::filetime_now());
        let health_before = target.health;
        let wait_ms = ((crate::space::dist(from.pos, target.pos) / missile().velocity.max(1.0)).clamp(0.5, 30.0) * 1000.0) as u64 + 1000;
        let attempt = admitted(&f, guard, source, from.type_id).await?;
        let (owner, ship) = (f.owner, f.ship);
        ensure!(f.world.with(f.system, move |s| s.drone_order(source, owner, ship, DroneOrder::ReturnHome)).await == Some(true), "recall failed");
        ensure!(f.world.with(f.system, move |s| !guard.valid(s)).await == Some(true), "recall did not invalidate captured guard");
        let before = f.world.with(f.system, |s| s.balls().count()).await.ok_or_else(|| anyhow!("actor stopped"))?;
        weapons::launch_missile(f.db.clone(), f.world.clone(), f.system, from.clone(), target.clone(), f.owner, None, missile(), Some(guard), attempt).await.ok().ok_or_else(|| anyhow!("stale launch failed"))?;
        ensure!(f.world.with(f.system, |s| s.balls().count()).await == Some(before), "stale missile inserted");
        ensure!(f.world.after(f.system, wait_ms).await, "actor stopped");
        ensure!(f.world.with(f.system, move |s| s.balls().count() == before && s.ball(victim).is_some_and(|b| b.health == health_before)).await == Some(true), "stale launch changed actor or target health");
        while let Ok((event, _)) = rx_victim.try_recv() {
            ensure!(event != "OnDamageMessage" && event != "OnCrimewatchEngagementCreated", "stale launch was accepted");
        }
        while let Ok((event, _)) = rx_owner.try_recv() {
            ensure!(!matches!(event.as_str(), "OnWeaponsTimerUpdate" | "OnPvpTimerUpdate"
                | "OnCrimewatchEngagementCreated" | "OnCriminalTimerUpdate"
                | "OnSecurityStatusUpdate" | "OnSystemCriminalFlagUpdates"),
                "refused launch sent attacker crimewatch notification: {event}");
        }
        ensure!(crate::services::crimewatch::security_status(&f.db, f.owner).await.ok() == Some(security_before), "refused launch changed security status");
        ensure!(crate::services::crimewatch::weapons_timer(f.owner) == weapons_before, "refused launch changed weapons timer");
        let now = crate::client::filetime_now();
        ensure!(crate::services::crimewatch::retaliation_allowed(other, f.owner, now) == retaliation_before
            && crate::services::crimewatch::retaliation_allowed(f.owner, other, now) == reverse_before, "refused launch changed engagement or flags");
        let current = locked(&f, source, victim).await?;
        let (from, target) = f.world.with(f.system, move |s| Some((s.sampled_ball(source)?, s.sampled_ball(victim)?))).await.flatten().ok_or_else(|| anyhow!("missing drones"))?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx);
        let attempt = admitted(&f, current, source, from.type_id).await?;
        weapons::launch_missile(f.db.clone(), f.world.clone(), f.system, from, target, f.owner, None, missile(), Some(current), attempt).await.ok().ok_or_else(|| anyhow!("valid launch failed"))?;
        ensure!(f.world.with(f.system, |s| s.balls().count()).await == Some(before + 1), "valid missile not inserted");
        ensure!(f.world.with(f.system, move |s| s.drone_order(source, owner, ship, DroneOrder::ReturnHome)).await == Some(true), "post-launch recall failed");
        tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                while let Ok((event, args)) = rx.try_recv() {
                    if event == "OnDamageMessage" && has_field(&args, "target", victim) {
                        if let Some(Value::Dict(fields)) = args.first() {
                            if fields.iter().any(|(k, v)| *k == Value::str("damage") && matches!(v, Value::Float(d) if *d > 0.0)) { return Ok(()); }
                        }
                    }
                }
                ensure!(f.world.after(f.system, 100).await, "actor stopped awaiting impact");
            }
        }).await.map_err(|_| anyhow!("in-flight impact timed out"))??;
        Ok(())
    }).await;
    f.cleanup().await;
    match result { Ok(result) => result, Err(panic) => std::panic::resume_unwind(panic) }
}
