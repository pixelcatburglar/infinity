//! Launches persistent drone identities from the active ship into space.
//! Each source stack is committed separately; unsupported work effects may still launch physically.

use std::collections::HashSet;
use marshal::Value;
use crate::client::Client;
use crate::consts::{dogma::ATTR_DRONE_BANDWIDTH, dogma::ATTR_MAX_ACTIVE_DRONES, CATEGORY_DRONE, CATEGORY_SHIP, FLAG_DRONE_BAY, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::dao::items::{drones, loading::ItemQuery, ship as ship_dao};
use crate::db::outbox::{dispatch_client, Outbox};
use crate::messages::drones as text;
use crate::services::{db_err, notify_error, Call, CallError, CallResult, fit::{Fit, Key}, items::load_items};
use crate::services::combat::drone_health;
use crate::space::MODE_WARP;

// Client moniker groupSolarSystem (eveMisc.LaunchFromShip).
const GROUP_SOLAR_SYSTEM: i64 = 5;

fn parse(args: &[Value]) -> Result<(Vec<(i64, i32)>, Option<i64>), CallError> {
    let invalid = || notify_error(text::INVALID_COMMAND);
    let [Value::List(rows), behalf, warning] = args else { return Err(invalid()) };
    let behalf = match behalf {
        Value::None => None,
        Value::Int(id) if *id > 0 => Some(*id),
        _ => return Err(invalid()),
    };
    if !matches!(warning, Value::Bool(_) | Value::Int(0 | 1)) { return Err(invalid()); }
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Tuple(pair) = row else { return Err(invalid()) };
        let [Value::Int(id), Value::Int(count)] = pair.as_slice() else { return Err(invalid()) };
        let count = i32::try_from(*count).ok().filter(|v| *v > 0).ok_or_else(invalid)?;
        if *id <= 0 || !seen.insert(*id) { return Err(invalid()); }
        result.push((*id, count));
    }
    Ok((result, behalf))
}

fn error_value(error: CallError) -> Result<Value, CallError> {
    match error {
        CallError::User { msg, dict } => Ok(Value::list([Value::tuple([Value::str(msg), Value::Dict(dict.into_iter().map(|(k, v)| (Value::str(k), v)).collect())])])),
        error => Err(error),
    }
}

fn check_count(count: i32, max_active: f64) -> Result<(), CallError> {
    if count <= 0 || !max_active.is_finite() || max_active < f64::from(count) {
        return Err(notify_error(text::MAX_ACTIVE_EXCEEDED));
    }
    Ok(())
}

pub(crate) async fn launch(c: &mut Client, call: &Call<'_>) -> CallResult {
    let (requests, behalf) = parse(call.args)?;
    let (Some(owner), Some(ship), Some(system)) = (c.session.int("charid"), c.session.int("shipid"), c.session.int("solarsystemid")) else { return Err(notify_error(text::NOT_IN_SPACE)) };
    if behalf.is_some_and(|id| id != owner) { return Err(notify_error(text::NOT_CONTROLLED)); }
    let bound = call.bind_params.and_then(Value::items);
    if !call.bound || c.session.int("stationid").is_some() || !bound.is_some_and(|v| v.len() == 2 && v[0].as_int() == Some(system) && v[1].as_int() == Some(GROUP_SOLAR_SYSTEM)) {
        return Err(notify_error(text::NOT_IN_SPACE));
    }
    let _lock = crate::services::market::lock().await;
    let mut result = Vec::with_capacity(requests.len());
    for (id, count) in requests {
        let value = match launch_one(c, owner, ship, system, id, count).await {
            Ok(ids) => Value::list(ids.into_iter().map(Value::Int)),
            Err(error) => error_value(error)?,
        };
        result.push((Value::Int(id), value));
    }
    Ok(Value::Dict(result))
}

async fn launch_one(c: &mut Client, owner: i64, ship: i64, system: i64, id: i64, count: i32) -> Result<Vec<i64>, CallError> {
    let unavailable = || notify_error(text::NOT_AVAILABLE);
    let item = load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
    if item.owner_id != owner || item.location_id != ship || item.flag_id != FLAG_DRONE_BAY || item.category_id != i64::from(CATEGORY_DRONE) || count > item.quantity || (item.singleton && (item.quantity != 1 || count != 1)) { return Err(unavailable()); }
    let (pos, affiliation) = c.world.with(system, move |s| s.ball(ship).filter(|b| b.char_id == Some(owner) && b.mode != MODE_WARP && b.cloak == 0).map(|b| (b.position_at(s.now_ms()), (b.corp_id, b.alliance_id)))).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?.ok_or_else(|| notify_error(text::NOT_IN_SPACE))?;
    let mut fit = Fit::drone(&c.db, c.sde.clone(), owner, id).await?;
    let key = Key::Drone(id);
    crate::services::effects::fitting::required_skills(&fit, &c.sde, key, item.type_id, false)?;
    let max_active = fit.value(Key::Char, ATTR_MAX_ACTIVE_DRONES);
    let total_bandwidth = fit.value(Key::Ship, ATTR_DRONE_BANDWIDTH);
    check_count(count, max_active)?;
    let original_spec = super::super::worker::spec(&fit, key, item.type_id)?;
    let original_seed = drone_health::prepare(&c.db, &fit, key, id, true).await?;
    let original_ball = super::drone_ball(&fit, key, id, item.type_id, owner, affiliation, pos, original_seed.fractions())?;
    fit.include_drone(id, item.type_id, Vec::new());
    crate::services::effects::fitting::required_skills(&fit, &c.sde, key, item.type_id, false)?;
    let pristine_spec = super::super::worker::spec(&fit, key, item.type_id)?;
    let pristine_seed = drone_health::prepare(&c.db, &fit, key, id, false).await?;
    let pristine_ball = super::drone_ball(&fit, key, id, item.type_id, owner, affiliation, pos, pristine_seed.fractions())?;
    let retains_original = count == item.quantity;
    let mut bandwidths = vec![pristine_spec.bandwidth; count as usize];
    if retains_original { bandwidths[0] = original_spec.bandwidth; }
    let token = c.world.with(system, move |s| s.reserve_drone_launch(owner, ship, &bandwidths, max_active, total_bandwidth)).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?.ok_or_else(|| notify_error(text::BANDWIDTH_EXCEEDED))?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let expected = item.clone();
    let world = c.world.clone();
    let effects = super::super::worker::effects(db.clone(), std::sync::Arc::downgrade(&world), system);
    let committed = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            let expected = expected.clone();
            Box::pin(async move {
                if !ship_dao::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(unavailable()); }
                if crate::dao::characters::selection::ship_id(tx, owner).await.map_err(db_err)? != Some(ship) { return Err(unavailable()); }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
                if hull.owner_id != owner || hull.location_id != system || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton { return Err(unavailable()); }
                let fresh = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
                if fresh.owner_id != owner || fresh.location_id != ship || fresh.flag_id != FLAG_DRONE_BAY || fresh.category_id != i64::from(CATEGORY_DRONE) || fresh.type_id != expected.type_id || fresh.quantity != expected.quantity || fresh.singleton != expected.singleton { return Err(unavailable()); }
                let ids = drones::launch(tx, &fresh, count, owner, ship, system, pos).await.map_err(db_err)?.ok_or_else(unavailable)?;
                let mut change = vec![(IX_STACK_SIZE, Value::Int(i64::from(fresh.quantity)))];
                if retains_original {
                    change.push((IX_LOCATION_ID, Value::Int(ship)));
                    change.push((IX_FLAG, Value::Int(i64::from(FLAG_DRONE_BAY))));
                    if !fresh.singleton { change.push((IX_SINGLETON, Value::Bool(false))); }
                }
                crate::services::inventory::notifications::notify_item_change(tx, &sde, outbox, id, change).await?;
                for &new_id in ids.iter().filter(|&&new_id| new_id != id) {
                    crate::services::inventory::notifications::notify_item_change(tx, &sde, outbox, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                }
                Ok(ids)
            })
        },
        |ids, outbox| async move {
            let task_ids = ids.clone();
            let task = tokio::spawn(async move {
                let mut entries = Vec::with_capacity(task_ids.len());
                for new_id in task_ids {
                    let (mut ball, spec, seed) = if new_id == id { (original_ball.clone(), original_spec.clone(), original_seed.clone()) } else { (pristine_ball.clone(), pristine_spec.clone(), pristine_seed.clone()) };
                    ball.id = new_id;
                    drone_health::install(new_id, seed);
                    entries.push((ball, spec));
                }
                let fallback_entries = entries.clone();
                let fallback_effects = effects.clone();
                match world.with(system, move |s| s.complete_drone_launch(token, owner, ship, entries, effects)).await {
                    Some(ack) => {
                        if ack.await.is_err() { tracing::warn!(system, "drone launch announcement lost after commit"); }
                    }
                    None => match world.system(system).await {
                        Ok(()) => {
                            match world.with(system, move |s| s.complete_drone_launch(token, owner, ship, fallback_entries, fallback_effects)).await {
                                Some(ack) => if ack.await.is_err() { tracing::warn!(system, "restored drone launch announcement lost"); },
                                None => tracing::warn!(system, "system actor stopped during committed launch restore"),
                            }
                        }
                        Err(error) => tracing::warn!(system, %error, "failed to restore committed drone launch"),
                    }
                }
            });
            Ok((ids, outbox, task))
        },
    ).await;
    match committed {
        Ok((ids, outbox, task)) => {
            if let Err(error) = task.await { tracing::warn!(system, %error, "committed drone launch completion failed"); }
            if let Err(error) = dispatch_client((), outbox, c) {
                tracing::warn!(system, ?error, "committed drone launch notification failed");
            }
            Ok(ids)
        }
        Err(error) => { let _ = c.world.with(system, move |s| s.cancel_drone_launch(token)).await; Err(error) }
    }
}

#[cfg(test)] mod tests;
