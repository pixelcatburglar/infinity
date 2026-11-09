use anyhow::{anyhow, ensure, Context, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::{Call, effects::{registry, EffectKind, registry::EffectAttribute}, fit::{Fit, Key}, mining}, space::{asteroids::Field, dist, drone::{DroneOrder, PendingMining}}};
use super::super::fixture::Fixture;

// SDE dgmAttributeTypes used by the shared mining calculation.
const MINING_AMOUNT: i32 = 77;
const WASTE_PROBABILITY: i32 = 3154;
const CRIT_CHANCE: i32 = 5967;

async fn tick(f: &Fixture) -> Result<()> {
    ensure!(f.world.after(f.system, 100).await, "system stopped");
    Ok(())
}

async fn state(f: &Fixture, id: i64, rock: i64) -> String {
    let ship = f.ship;
    f.world.with(f.system, move |s| {
        let gap = |a, b| s.sampled_ball(a).zip(s.sampled_ball(b))
            .map(|(a, b)| dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius));
        match s.drone(id) {
            Some(d) => format!("order={:?} state={} target={:?} controller={:?} generation={} pending={:?} rock_gap={:?} ship_gap={:?} action_range={} control_range={} target_range={} scan={} duration={} effect={:?} motion={:?} inertia={:?} system_time={}",
                d.order, d.state, d.target, d.controller, d.generation, d.pending, gap(id, rock), gap(id, ship),
                d.spec.optimal_range, d.spec.control_range, d.spec.max_target_range, d.spec.scan_resolution, d.spec.duration_ms, d.spec.effect_id,
                s.sampled_ball(id).map(|b| (b.pos, b.vel, b.mode, b.follow_id, b.follow_range, b.max_vel, b.goto)),
                s.ball(id).map(|b| (b.mass, b.agility, b.free, b.last_ms)), s.current_time()),
            None => format!("drone {id} absent; rock_gap={:?} ship_gap={:?}", gap(id, rock), gap(id, ship)),
        }
    }).await.unwrap_or_else(|| "system stopped".to_string())
}

async fn pending(f: &Fixture, id: i64, rock: i64, phase: &str) -> Result<PendingMining> {
    for _ in 0..600 {
        if let Some(intent) = f.world.with(f.system, move |s| s.drone(id).and_then(|d| d.pending)).await.context("system stopped")? {
            let ship = f.ship;
            let separated = f.world.with(f.system, move |s| s.sampled_ball(id).zip(s.sampled_ball(ship)).is_some_and(|(a, b)| dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius) > DRONE_SCOOP_RANGE)).await.context("system stopped")?;
            ensure!(separated, "pending cycle was already at ship contact");
            return Ok(intent);
        }
        tick(f).await?;
    }
    Err(anyhow!("{phase}: mining intent did not arrive: {}", state(f, id, rock).await))
}

async fn quantities(f: &Fixture, rock: i64, ore: i32) -> Result<(i64, i64)> {
    let remaining = f.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &f.system]).await?.get("quantity");
    let held = f.db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS held FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.owner, &f.ship, &FLAG_CARGO, &ore]).await?.get("held");
    Ok((remaining, held))
}

fn expected(intent: &PendingMining, remaining: i64) -> Result<i64> {
    let result = mining::rounding::mining_result(intent.inputs, intent.unit_volume, remaining, intent.draws)
        .map_err(|error| anyhow!("{error}"))?;
    ensure!(result.wasted_quantity == 0 && result.critical_hit_quantity == 0, "fixture produced waste or critical yield");
    Ok((result.normal_quantity + result.critical_hit_quantity).min(remaining - result.wasted_quantity))
}

async fn command(f: &mut Fixture, id: i64, method: &str, target: Option<i64>) -> Result<()> {
    let mut args = vec![Value::list([Value::Int(id)])];
    if let Some(target) = target { args.push(Value::Int(target)); }
    let response = f.call("entity", method, &args).await.map_err(|e| anyhow!("{method}: {e:?}"))?;
    ensure!(response == Value::Dict(vec![]), "{method} refused: {response:?}");
    Ok(())
}

async fn launch(f: &mut Fixture, id: i64) -> Result<()> {
    let args = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let response = f.call("ship", "LaunchDrones", &args).await.map_err(|e| anyhow!("launch: {e:?}"))?;
    ensure!(response == Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]), "launch refused: {response:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn physical_drone_mining_and_laser_share_extraction() -> Result<()> {
    let mut f = Fixture::new().await;
    let result: Result<()> = async {
        let rocks = mining::grow_field(&f.db, &f.world, f.system, f.ship, [0.0; 3], Field::Ore, 1.0, false).await.map_err(|e| anyhow!("growing field: {e:?}"))?;
        let rock = *rocks.first().context("generated field has no rocks")?;
        // The field is randomly placed; isolate the real target so unrelated rocks
        // cannot obstruct the fixture's outbound and return journeys.
        for &other in rocks.iter().skip(1) {
            super::remove_fixture_rock(&f.db, &f.world, f.system, f.ship, other).await.context("isolating target rock")?;
        }
        let ball = f.world.ball(f.system, rock).await.context("rock has no ball")?;
        let ore = ball.type_id;
        let volume = f.sde.types.get(ore).and_then(|t| t.volume).unwrap_or(1.0).max(0.01);
        let (ty, duration_attr, range_attr) = f.sde.types.iter().filter(|t| t.name.as_deref() == Some("Mining Drone I") && t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE) && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).find_map(|t| {
            t.effects().iter().filter_map(|e| registry::resolve(&f.sde, e.effect_id)).find(|e| matches!(e.kind,
                EffectKind::Turret | EffectKind::Missile | EffectKind::Mining | EffectKind::RemoteRepair { .. } |
                EffectKind::Neutralizer | EffectKind::Ecm | EffectKind::Projected { jamming: crate::services::effects::JammingType::Webify | crate::services::effects::JammingType::Paint | crate::services::effects::JammingType::Damp | crate::services::effects::JammingType::TrackingDisrupt }))
                .filter(|e| e.kind == EffectKind::Mining).map(|e| (t.type_id, e.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE), e.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE)))
        }).context("published mining drone unavailable")?;
        f.train_drone(ty).await;
        let id = f.drone(ty, 1, true).await;
        for (item, attr, value) in [(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0), (f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0),
            (f.ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0), (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (f.ship, ATTR_GENERAL_MINING_HOLD_CAPACITY, 0.0), (f.ship, ATTR_CAPACITY, volume * 100.0),
            (id, ATTR_MAX_VELOCITY, 2_000.0), (id, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (id, ATTR_SCAN_RESOLUTION, 1_000_000.0), (id, ATTR_MAX_RANGE, 500.0), (id, range_attr, 500.0),
            (id, duration_attr, 1_000.0), (id, MINING_AMOUNT, volume * 2.0), (id, WASTE_PROBABILITY, 0.0), (id, CRIT_CHANCE, 0.0)] { f.set(item, attr, value).await; }
        let fit = Fit::drone(&f.db, f.sde.clone(), f.owner, id).await.map_err(|e| anyhow!("drone fit: {e:?}"))?;
        let (mass, agility) = (fit.value(Key::Drone(id), ATTR_MASS), fit.value(Key::Drone(id), ATTR_AGILITY));
        ensure!(mass == DRONE_FALLBACK_MASS && agility == DRONE_FALLBACK_AGILITY,
            "mining drone fitted inertia differs from policy: type={ty} mass={mass} agility={agility}");
        let ship = f.ship;
        let ship_radius = f.world.ball(f.system, ship).await.context("ship has no ball")?.radius;
        let position = [ball.pos[0] + f64::from(ball.radius) + f64::from(ship_radius) + 8_100.0, ball.pos[1], ball.pos[2]];
        f.world.with(f.system, move |s| s.set_ball_position(ship, position, |_, _| true)).await.context("system stopped")?;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if f.world.with(f.system, move |s| s.sampled_ball(ship).is_some_and(|b| b.pos == position)).await.context("system stopped positioning ship")? {
                    return Ok::<(), anyhow::Error>(());
                }
                tick(&f).await?;
            }
        }).await.map_err(|_| anyhow!("ship position was not applied: expected {position:?}"))??;
        let gap = f.world.with(f.system, move |s| s.sampled_ball(ship).zip(s.sampled_ball(rock))
            .map(|(ship, rock)| dist(ship.pos, rock.pos) - f64::from(ship.radius) - f64::from(rock.radius)))
            .await.context("system stopped")?.context("ship or rock missing after positioning")?;
        ensure!(gap >= 8_000.0, "ship started too close to rock: surface gap {gap}");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx);
        launch(&mut f, id).await?;
        let motion = f.world.ball(f.system, id).await.context("launched drone has no ball")?;
        ensure!(motion.free && motion.mass == DRONE_FALLBACK_MASS && f64::from(motion.agility) == DRONE_FALLBACK_AGILITY,
            "launched drone inertia differs from fitted policy: type={ty} mass={} agility={} free={} last_ms={}", motion.mass, motion.agility, motion.free, motion.last_ms);
        let args = [Value::Int(rock)]; let kwargs = Value::None;
        let lock = Call { service: "dogmaIM", method: "AddTarget", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dogma::dispatch(&mut f.client, &lock).await.map_err(|e| anyhow!("target lock: {e:?}"))?;
        let mut locked = false;
        for _ in 0..300 { if crate::services::combat::targeting_charges::targets_of(f.owner).contains(&rock) { locked = true; break; } tick(&f).await?; }
        ensure!(locked, "rock did not lock: {}", state(&f, id, rock).await);
        let initial = quantities(&f, rock, ore).await?;
        let baseline = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        f.set(id, MINING_AMOUNT, 0.0).await;
        command(&mut f, id, "CmdMineRepeatedly", Some(rock)).await?;
        let zero = pending(&f, id, rock, "zero-yield intent").await?;
        ensure!(expected(&zero, initial.0)? == 0, "zero-yield intent was not zero");
        let mut resumed = false;
        for _ in 0..600 {
            if f.world.with(f.system, move |s| s.drone(id).is_some_and(|d| d.order == DroneOrder::Mine(rock) && d.pending.is_none())).await.context("system stopped")? { resumed = true; break; }
            tick(&f).await?;
        }
        ensure!(resumed, "zero-yield contact did not resume mining: {}", state(&f, id, rock).await);
        ensure!(quantities(&f, rock, ore).await? == initial, "zero-yield contact changed inventory");
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == baseline, "zero-yield contact recorded a fact");
        while let Ok((event, _)) = rx.try_recv() { ensure!(event != "OnOreMined", "zero-yield contact emitted ore notice"); }
        f.set(id, MINING_AMOUNT, volume * 2.0).await;
        let first = pending(&f, id, rock, "full-hold intent").await?;
        ensure!(first.rock == rock && expected(&first, initial.0)? > 0, "unexpected mining intent");
        let travelled = f.world.with(f.system, move |s| s.sample_position(id).zip(s.sample_position(ship))
            .map(|(drone, ship)| dist(drone, ship))).await.context("system stopped")?.context("drone or ship missing after mining")?;
        ensure!(travelled >= 7_000.0, "mining intent did not follow the physical journey: ship distance {travelled}");
        ensure!(quantities(&f, rock, ore).await? == initial, "pending intent mutated inventory");
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == baseline, "pending intent recorded a fact");
        while let Ok((event, _)) = rx.try_recv() { ensure!(event != "OnOreMined", "notice before contact"); }
        f.set(f.ship, ATTR_CAPACITY, 0.0).await;
        let mut idle = false;
        for _ in 0..600 {
            if f.world.with(f.system, move |s| s.drone(id).is_some_and(|d| d.order == DroneOrder::Idle && d.pending.is_none())).await.context("system stopped")? { idle = true; break; }
            tick(&f).await?;
        }
        ensure!(idle && quantities(&f, rock, ore).await? == initial, "full hold did not reject intent unchanged: {}", state(&f, id, rock).await);
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == baseline, "full hold recorded a fact");
        while let Ok((event, _)) = rx.try_recv() { ensure!(event != "OnOreMined", "full hold emitted ore notice"); }
        f.set(f.ship, ATTR_CAPACITY, volume * 100.0).await;
        command(&mut f, id, "CmdMineRepeatedly", Some(rock)).await?;
        let second = pending(&f, id, rock, "delivery intent").await?;
        ensure!(quantities(&f, rock, ore).await? == initial, "second intent mutated inventory");
        let second_units = expected(&second, initial.0)?;
        let mut delivered = false;
        for _ in 0..600 { if quantities(&f, rock, ore).await? != initial { delivered = true; break; } tick(&f).await?; }
        ensure!(delivered, "contact did not deliver: {}", state(&f, id, rock).await);
        let mut completed = false;
        for _ in 0..100 {
            if f.world.with(f.system, move |s| s.drone(id).is_some_and(|d| d.pending.is_none())).await.context("system stopped")? { completed = true; break; }
            tick(&f).await?;
        }
        ensure!(completed, "committed contact did not release the intent: {}", state(&f, id, rock).await);
        let after = quantities(&f, rock, ore).await?;
        ensure!(initial.0 - after.0 == second_units && after.1 - initial.1 == second_units, "delivery differs from pending result");
        let first_logs = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        ensure!(first_logs.len() == baseline.len() + 1, "contact did not record exactly one fact");
        ensure!((first_logs[0].type_id, first_logs[0].solar_system_id, first_logs[0].quantity, first_logs[0].quantity_wasted) == (ore, f.system, second_units, 0), "contact fact differs from delivery");
        command(&mut f, id, "CmdReturnHome", None).await?;
        let mut notices = 0;
        for _ in 0..100 { while let Ok((event, _)) = rx.try_recv() { if event == "OnOreMined" { notices += 1; } } if notices > 0 { break; } tick(&f).await?; }
        ensure!(notices == 1 && quantities(&f, rock, ore).await? == after, "post-commit notice or single delivery missing");
        command(&mut f, id, "CmdMineRepeatedly", Some(rock)).await?;
        let _rollback_intent = pending(&f, id, rock, "rejected-bay intent").await?;
        f.set(f.ship, ATTR_DRONE_CAPACITY, 0.0).await;
        command(&mut f, id, "CmdReturnBay", None).await?;
        let mut rejected = false;
        for _ in 0..600 {
            if f.world.with(f.system, move |s| s.drone(id).is_some_and(|d| d.order == DroneOrder::Idle && d.pending.is_none())).await.context("system stopped")? { rejected = true; break; }
            tick(&f).await?;
        }
        ensure!(rejected && quantities(&f, rock, ore).await? == after, "failed bay return harvested ore or did not complete: {}", state(&f, id, rock).await);
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == first_logs, "failed bay return recorded a fact");
        let row = f.db.query_one("SELECT location_id FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&id, &f.owner]).await?;
        ensure!(row.get::<_, i64>("location_id") == f.system, "failed bay return moved drone");
        while let Ok((event, _)) = rx.try_recv() { ensure!(event != "OnOreMined", "failed bay return emitted ore notice"); }
        f.set(f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        command(&mut f, id, "CmdMineRepeatedly", Some(rock)).await?;
        let third = pending(&f, id, rock, "bay-delivery intent").await?;
        let before_return = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        ensure!(before_return == first_logs, "cached bay intent appended a source before return");
        f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(mining::progress::aggregate_tx(tx, outbox, 100)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow!("aggregate before return: {e:?}"))?;
        let before_events = crate::db::outbox::tests::events(&f.db, "OreMined", f.owner).await;
        ensure!(before_events.len() == 1, "cached intent created a journal event");
        let third_units = expected(&third, after.0)?;
        command(&mut f, id, "CmdReturnBay", None).await?;
        let mut bay = false;
        for _ in 0..600 {
            let row = f.db.query_one("SELECT location_id,flag_id FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&id, &f.owner]).await?;
            if row.get::<_, i64>("location_id") == f.ship && row.get::<_, i32>("flag_id") == FLAG_DRONE_BAY { bay = true; break; }
            tick(&f).await?;
        }
        ensure!(bay, "pending return did not reach bay: {}", state(&f, id, rock).await);
        let returned = quantities(&f, rock, ore).await?;
        ensure!(after.0 - returned.0 == third_units && returned.1 - after.1 == third_units, "return did not join mining delivery with bay move");
        let returned_logs = crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await?;
        ensure!(returned_logs.len() == first_logs.len() + 1, "bay return did not record exactly one fact");
        ensure!((returned_logs[0].type_id, returned_logs[0].solar_system_id, returned_logs[0].quantity, returned_logs[0].quantity_wasted) == (ore, f.system, third_units, 0), "bay fact differs from delivery");
        let source = f.db.query_one("SELECT log_id, event_time, progress_aggregated FROM game.character_mining_ledger WHERE character_id=$1 ORDER BY log_id DESC LIMIT 1", &[&f.owner]).await?;
        let source_id: i64 = source.get("log_id");
        let source_time: i64 = source.get("event_time");
        ensure!(!source.get::<_, bool>("progress_aggregated"), "return source was already aggregated");
        f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(mining::progress::aggregate_tx(tx, outbox, 100)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow!("aggregate return: {e:?}"))?;
        let events = crate::db::outbox::tests::events(&f.db, "OreMined", f.owner).await;
        let slices = events.iter().filter_map(|event| match event {
            crate::lua::events::GameEvent::OreMined { ore_type, slices, .. } if *ore_type == ore => Some(slices),
            _ => None,
        }).flatten().filter(|slice| slice.log_id == source_id).collect::<Vec<_>>();
        ensure!(events.len() == 2 && slices.len() == 1 && slices[0].occurred_at == source_time && slices[0].quantity == third_units && slices[0].volume_m3 == third.unit_volume * third_units as f64, "return journal does not contain exactly the delivered source slice");
        let mut return_notices = 0;
        for _ in 0..100 {
            while let Ok((event, _)) = rx.try_recv() { if event == "OnOreMined" { return_notices += 1; } }
            if return_notices > 0 { break; }
            tick(&f).await?;
        }
        ensure!(return_notices == 1, "return delivery did not emit exactly one post-commit ore notice");
        for _ in 0..100 { if f.world.ball(f.system, id).await.is_none() { break; } tick(&f).await?; }
        ensure!(f.world.ball(f.system, id).await.is_none(), "returned drone still in space");
        ensure!(command(&mut f, id, "CmdReturnBay", None).await.is_err(), "already returned drone accepted another return");
        f.db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(mining::progress::aggregate_tx(tx, outbox, 100)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow!("aggregate repeated return: {e:?}"))?;
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == returned_logs && crate::db::outbox::tests::events(&f.db, "OreMined", f.owner).await.len() == 2, "repeated return created another source or event");
        launch(&mut f, id).await?;
        command(&mut f, id, "CmdMineRepeatedly", Some(rock)).await?;
        let _lost = pending(&f, id, rock, "control-loss intent").await?;
        let ship = f.ship;
        f.world.with(f.system, move |s| s.drone_controller_lost(ship)).await.context("system stopped")?;
        ensure!(f.world.with(f.system, move |s| s.drone(id).is_some_and(|d| d.controller.is_none() && d.pending.is_none())).await.context("system stopped")?, "control loss retained intent");
        for _ in 0..20 { tick(&f).await?; }
        ensure!(quantities(&f, rock, ore).await? == returned, "control loss harvested pending intent");
        ensure!(crate::dao::mining_ledger::list(&f.db, f.owner, i64::MIN, i64::MAX).await? == returned_logs, "control loss recorded a fact");
        while let Ok((event, _)) = rx.try_recv() { ensure!(event != "OnOreMined", "control loss emitted ore notice"); }
        Ok(())
    }.await;
    let cleanup: Result<()> = async {
        let rows = f.db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await?;
        let mut errors = Vec::new();
        for row in rows { if let Err(e) = super::remove_fixture_rock(&f.db, &f.world, f.system, f.ship, row.get("asteroid_id")).await { errors.push(format!("{e:?}")); } }
        if let Err(e) = f.db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&f.system, &f.ship]).await { errors.push(e.to_string()); }
        ensure!(errors.is_empty(), "field cleanup: {}", errors.join(", "));
        Ok(())
    }.await;
    f.cleanup().await;
    cleanup?;
    result
}
