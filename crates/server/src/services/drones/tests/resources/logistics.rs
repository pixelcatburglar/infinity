use anyhow::{anyhow, ensure, Result};
use marshal::Value;
use infinity_core::{
    consts::{*, dogma::*},
};
use crate::{
    services::{Call, CallError, combat::{self, damage}, drones::worker,
    effects::{registry::{self, EffectAttribute}, EffectKind}, fit::{Fit, Key}},
};
use super::{fleet::form_fleet, super::fixture::Fixture};

fn error(e: CallError) -> anyhow::Error {
    match e { CallError::Internal(e) => e, CallError::User { msg, .. } => anyhow!("RPC refused: {msg}") }
}

async fn health(f: &Fixture, ship: i64) -> Result<[f64; 3]> {
    Ok(f.world.ball(f.system, ship).await.ok_or_else(|| anyhow!("target absent"))?.health)
}

async fn persisted_health(f: &Fixture, item: i64) -> Result<String> {
    let attrs = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
    Ok(f.db.query_one("SELECT COALESCE(string_agg(row_to_json(t)::text, ',' ORDER BY attribute_id), '') AS state FROM (SELECT * FROM game.item_attributes WHERE item_id=$1 AND attribute_id=ANY($2)) t", &[&item, &attrs.as_slice()]).await?.get("state"))
}

async fn state(f: &Fixture, target: i64, drone: Option<i64>) -> String {
    let ship_ball = f.world.ball(f.system, f.ship).await
        .map(|b| (b.pos, b.radius, b.char_id, b.health));
    let target_ball = f.world.ball(f.system, target).await
        .map(|b| (b.pos, b.radius, b.signature_radius, b.health));
    let source_ball = match drone {
        Some(id) => f.world.ball(f.system, id).await.map(|b| (b.pos, b.radius)),
        None => None,
    };
    let record = match drone {
        Some(id) => f.world.with(f.system, move |s| s.drone(id).map(|d|
            (d.state, d.order, d.target, d.controller, d.spec.scan_resolution,
             d.spec.control_range, d.spec.max_target_range, d.spec.optimal_range, d.spec.duration_ms))).await.flatten(),
        None => None,
    };
    format!("owner={} ship={} ship_ball={ship_ball:?} target={target} target_ball={target_ball:?} drone={drone:?} source_ball={source_ball:?} record={record:?} targets={:?}",
        f.owner, f.ship, combat::targeting_charges::targets_of(f.owner))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn logistics_drone_repairs_fleet_pilot_layers() -> Result<()> {
    let mut f = Fixture::new().await;
    let mut result: Result<()> = Ok(());
    let panic = crate::tests::catch_scenario(async {
        result = async {
        let (other, target) = f.add_pilot().await;
        form_fleet(&mut f, other, target).await?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(other, tx);
        for (item, attr, value) in [
            (f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
            (f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0),
            (f.ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (target, ATTR_SHIELD_CAPACITY, 1000.0), (target, ATTR_ARMOR_HP, 1000.0),
            (target, ATTR_STRUCTURE_HP, 1000.0), (target, ATTR_SHIELD_RECHARGE_RATE, 1e30),
            (target, ATTR_SHIELD_EM_DAMAGE_RESONANCE, 1.0),
            (target, ATTR_ARMOR_EM_DAMAGE_RESONANCE, 1.0),
            (target, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, 1.0),
        ] { f.set(item, attr, value).await; }
        let target_fit = Fit::load_in_space(&f.db, &f.world, f.system, other, target).await.map_err(error)?;
        for attr in [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP] {
            ensure!((target_fit.value(Key::Ship, attr) - 1000.0).abs() < 1e-6, "unexpected fitted target HP");
        }
        let (generation, revision, projection) = f.world.with(f.system, move |s| Some((s.combat_membership_generation(target)?, s.combat_fit_revision(target)?, s.projection_revision(target)))).await.flatten().ok_or_else(|| anyhow!("missing target fit cache"))?;
        ensure!(f.world.with(f.system, move |s| s.replace_fitted_cache(target, generation, revision, target_fit, Key::Ship, Some(projection))).await == Some(true), "target fit publication failed");
        // Resizing preserves absolute HP, so the fixture explicitly starts full.
        let condition = f.world.with(f.system, move |s| {
            s.set_condition_fraction(target, 1.0);
            s.combat_condition_snapshot(target).map(|snapshot| snapshot.condition)
        }).await.flatten().ok_or_else(|| anyhow!("missing target condition"))?;
        ensure!(condition.now == condition.stats.max, "target condition is not full: current={:?}, max={:?}", condition.now, condition.stats.max);
        let source_fit = Fit::load_in_space(&f.db, &f.world, f.system, f.owner, f.ship).await.map_err(error)?;
        let scan = source_fit.value(Key::Ship, ATTR_SCAN_RESOLUTION);
        let bay_capacity = source_fit.value(Key::Ship, ATTR_DRONE_CAPACITY);
        ensure!(bay_capacity.is_finite() && bay_capacity > 0.0, "invalid fitted drone bay capacity: {bay_capacity}");
        let target_ball = f.world.ball(f.system, target).await.ok_or_else(|| anyhow!("target absent before lock"))?;
        // AddTarget uses the type signature, falling back to the ball radius, not its dogma override.
        let signature = f.sde.types.attribute(target_ball.type_id, ATTR_SIGNATURE_RADIUS).unwrap_or(f64::from(target_ball.radius));
        let lock_ms = crate::space::lock_time_ms(scan, signature);
        ensure!(scan > 0.0 && lock_ms <= 1000, "unexpected fitted lock time: scan={scan}, signature={signature}, delay={lock_ms}ms");
        let persisted = persisted_health(&f, target).await?;
        let source = f.ship;
        let owner = f.owner;
        let system = f.system;
        let weak = std::sync::Arc::downgrade(&f.world);
        let generation = crate::space::targeting::next_generation();
        let mut attempt = f.world.with(system, move |s| {
            let facts = s.prepare_combat_source(source, None, None, None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1,
                move |s| s.pilot_ball(owner).is_some_and(|ball| ball.id == source))?;
            Ok::<_, crate::space::combat_facts::Refusal>(crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system))
        }).await.ok_or_else(|| anyhow!("source actor unavailable"))?.map_err(|e| anyhow!("source admission: {e:?}"))?;
        let outcome = damage::apply_damage_guarded(&f.db, &f.world, system, target, [2500.0, 0.0, 0.0, 0.0],
            damage::DamageSource { facts: Some(attempt.facts()), ..Default::default() }, false, |_| true).await.map_err(error)?;
        ensure!(outcome.is_some(), "damage was not applied");
        attempt.disarm();
        ensure!(persisted_health(&f, target).await? == persisted, "damage checkpointed before periodic save");
        ensure!(f.world.after(f.system, 100).await, "system stopped after damage");
        let initial = health(&f, target).await?;
        ensure!(initial[0].abs() < 1e-6 && initial[1].abs() < 1e-6 && (initial[2] - 0.5).abs() < 1e-6, "damage did not reach all layers: {initial:?}");
        while rx.try_recv().is_ok() {}
        let kwargs = Value::None;
        let args = [Value::Int(target)];
        let lock = Call { service: "dogmaIM", method: "AddTarget", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dogma::dispatch(&mut f.client, &lock).await.map_err(error)?;
        let locked = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while !combat::targeting_charges::targets_of(f.owner).contains(&target) {
                ensure!(f.world.after(f.system, 100).await, "system stopped locking target");
            }
            Ok::<(), anyhow::Error>(())
        }).await;
        match locked {
            Ok(result) => result?,
            Err(_) => return Err(anyhow!("pilot lock timeout: scan={scan}, signature={signature}, delay={lock_ms}ms; {}", state(&f, target, None).await)),
        }
        let mut bay_volume = 0.0;
        for (layer, amount_attr) in [ATTR_SHIELD_BONUS, ATTR_ARMOR_DAMAGE_AMOUNT, ATTR_STRUCTURE_DAMAGE_AMOUNT].into_iter().enumerate() {
            let (ty, effect) = f.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE))
                .find_map(|t| {
                    let effect = t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id))
                        .find(|e| e.kind == (EffectKind::RemoteRepair { layer }))?;
                    Some((t.type_id, effect))
                }).ok_or_else(|| anyhow!("no published repair drone for layer {layer}"))?;
            let volume = f.sde.types.get(ty).and_then(|t| t.volume)
                .ok_or_else(|| anyhow!("repair drone type {ty} lacks volume"))?;
            ensure!(volume.is_finite() && volume >= 0.0, "invalid drone volume: type={ty}, volume={volume}");
            bay_volume += volume;
            ensure!(bay_volume.is_finite() && bay_volume <= bay_capacity,
                "repair drones exceed fitted bay: layer={layer}, type={ty}, total={bay_volume}, capacity={bay_capacity}");
            f.train_drone(ty).await;
            let drone = f.drone(ty, 1, true).await;
            let duration_attr = effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE);
            let range_attr = effect.attribute(EffectAttribute::Range).ok_or_else(|| anyhow!("repair effect lacks range attribute"))?;
            for (attr, value) in [(duration_attr, 2000.0), (range_attr, 1_000_000.0), (amount_attr, 100.0), (ATTR_SCAN_RESOLUTION, 1_000_000.0)] {
                f.set(drone, attr, value).await;
            }
            let fit = Fit::drone(&f.db, f.sde.clone(), f.owner, drone).await.map_err(error)?;
            let spec = worker::spec(&fit, Key::Drone(drone), ty).map_err(error)?;
            ensure!(spec.effect_id == Some(effect.id as i32) && (spec.duration_ms - 2000.0).abs() < 1e-6, "wrong selected drone effect or duration");
            ensure!(spec.scan_resolution > 0.0 && crate::space::lock_time_ms(spec.scan_resolution, 1.0) <= 1000,
                "unexpected drone lock: layer={layer}, drone={drone}, scan={}", spec.scan_resolution);
            let amount = fit.value(Key::Drone(drone), amount_attr);
            ensure!(amount > 0.0 && amount < 500.0 && fit.value(Key::Drone(drone), range_attr) > 100_000.0, "invalid fitted repair amount or range");
            let launch = [Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
            let launched = f.call("ship", "LaunchDrones", &launch).await.map_err(error)?;
            ensure!(launched == Value::Dict(vec![(Value::Int(drone), Value::list([Value::Int(drone)]))]), "launch failed: {launched:?}");
            ensure!(f.world.ball(f.system, drone).await.is_some_and(|b| b.char_id.is_none() && b.owner_id == f.owner), "source is not a physical unpiloted drone");
            let persisted = persisted_health(&f, target).await?;
            let before = health(&f, target).await?;
            let args = [Value::list([Value::Int(drone)]), Value::Int(target)];
            ensure!(f.call("entity", "CmdEngage", &args).await.map_err(error)? == Value::Dict(vec![]), "engage refused");
            let cycle = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                loop {
                    if f.world.with(f.system, move |s| s.drone(drone).is_some_and(|d| d.state == DRONE_STATE_COMBAT)).await.unwrap_or(false) { break; }
                    ensure!(f.world.after(f.system, 100).await, "system stopped before cycle");
                }
                Ok::<(), anyhow::Error>(())
            }).await;
            match cycle {
                Ok(result) => result?,
                Err(_) => return Err(anyhow!("cycle start timeout: layer={layer}, type={ty}, effect={}, amount={amount}, before={before:?}; {}", effect.id, state(&f, target, Some(drone)).await)),
            }
            if layer != 0 {
                ensure!(f.world.after(f.system, 200).await, "system stopped during cycle");
                let early = health(&f, target).await?;
                ensure!((early[layer] - before[layer]).abs() < 1e-7, "armor or hull repaired before cycle end");
            }
            let wait = if layer == 0 { std::time::Duration::from_millis(1500) } else { std::time::Duration::from_secs(20) };
            let after = tokio::time::timeout(wait, async {
                loop {
                    let now = health(&f, target).await?;
                    if now[layer] > before[layer] + 1e-6 { break Ok::<_, anyhow::Error>(now); }
                    ensure!(f.world.after(f.system, 100).await, "system stopped waiting for repair");
                }
            }).await;
            let after = match after {
                Ok(result) => result?,
                Err(_) => return Err(anyhow!("repair timeout: layer={layer}, phase={}, type={ty}, effect={}, amount={amount}, before={before:?}; {}", if layer == 0 { "begin" } else { "end" }, effect.id, state(&f, target, Some(drone)).await)),
            };
            let expected = (before[layer] + amount / 1000.0).min(1.0);
            ensure!((after[layer] - expected).abs() < 1e-6, "wrong layer repair: {before:?} -> {after:?}, expected {expected}");
            for i in 0..3 { if i != layer { ensure!((after[i] - before[i]).abs() < 1e-6, "unrelated layer changed"); } }
            if layer == 0 { ensure!(persisted_health(&f, target).await? == persisted, "repair checkpointed before periodic save"); }
            let mut hud = false;
            while let Ok((event, payload)) = rx.try_recv() {
                if event != "OnModuleAttributeChanges" { continue; }
                if let [Value::List(rows)] = payload.as_slice() {
                    let attr = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE][layer];
                    hud |= rows.iter().any(|row| matches!(row, Value::Tuple(parts) if parts.get(0) == Some(&Value::str("OnModuleAttributeChange")) && parts.get(1) == Some(&Value::Int(other)) && parts.get(2) == Some(&Value::Int(target)) && parts.get(3) == Some(&Value::Int(i64::from(attr)))));
                }
            }
            ensure!(hud, "target owner HUD notification missing");
            let args = [Value::list([Value::Int(drone)])];
            ensure!(f.call("entity", "CmdReturnBay", &args).await.map_err(error)? == Value::Dict(vec![]), "return refused");
            let returned = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                while f.world.ball(f.system, drone).await.is_some() {
                    ensure!(f.world.after(f.system, 100).await, "system stopped returning drone");
                }
                Ok::<(), anyhow::Error>(())
            }).await;
            match returned {
                Ok(result) => result?,
                Err(_) => return Err(anyhow!("return timeout: layer={layer}, type={ty}; {}", state(&f, target, Some(drone)).await)),
            }
            let row = f.db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1", &[&drone]).await?;
            ensure!(row.get::<_, i64>("location_id") == f.ship && row.get::<_, i32>("flag_id") == FLAG_DRONE_BAY,
                "drone not returned to ship drone bay");
            while rx.try_recv().is_ok() {}
        }
        Ok(())
        }.await;
    }).await;
    f.cleanup().await;
    if let Err(panic) = panic { std::panic::resume_unwind(panic); }
    result
}
