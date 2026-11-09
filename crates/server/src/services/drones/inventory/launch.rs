//! Launches persistent drone identities from the active ship into space.
//! Each source stack is committed separately; unsupported work effects may still launch physically.

use marshal::Value;
use crate::client::Client;
use infinity_core::consts::{dogma::ATTR_DRONE_BANDWIDTH, dogma::ATTR_MAX_ACTIVE_DRONES, CATEGORY_DRONE, CATEGORY_SHIP, FLAG_DRONE_BAY, GROUP_SOLAR_SYSTEM, IX_FLAG, IX_LOCATION_ID, IX_SINGLETON, IX_STACK_SIZE};
use crate::dao::items::{drones, loading::ItemQuery, ship as ship_dao};
use crate::db::outbox::{dispatch_world, Outbox};
use infinity_core::messages::drones as text;
use crate::services::{db_err, notify_error, Call, CallError, CallResult, fit::{Fit, Key}, items::load_items};
use crate::services::item_launch::{parse, error_value};
use crate::space::MODE_WARP;

fn check_count(count: i32, max_active: f64) -> Result<(), CallError> {
    if count <= 0 || !max_active.is_finite() || max_active < f64::from(count) {
        return Err(notify_error(text::MAX_ACTIVE_EXCEEDED));
    }
    Ok(())
}

pub(crate) async fn launch(c: &mut Client, call: &Call<'_>) -> CallResult {
    let (requests, behalf) = parse(call.args).map_err(|()| notify_error(text::INVALID_COMMAND))?;
    let (Some(owner), Some(ship), Some(system)) = (c.session.int("charid"), c.session.int("shipid"), c.session.int("solarsystemid")) else { return Err(notify_error(text::NOT_IN_SPACE)) };
    if behalf.is_some_and(|id| id != owner) { return Err(notify_error(text::NOT_CONTROLLED)); }
    let bound = call.bind_params.and_then(Value::items);
    if !call.bound || c.session.int("stationid").is_some() || !bound.is_some_and(|v| v.len() == 2 && v[0].as_int() == Some(system) && v[1].as_int() == Some(GROUP_SOLAR_SYSTEM)) {
        return Err(notify_error(text::NOT_IN_SPACE));
    }
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
    let market = crate::services::market::lock().await;
    let unavailable = || notify_error(text::NOT_AVAILABLE);
    let item = load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
    if item.owner_id != owner || item.location_id != ship || item.flag_id != FLAG_DRONE_BAY || item.category_id != i64::from(CATEGORY_DRONE) || i64::from(count) > item.quantity || (item.singleton && (item.quantity != 1 || count != 1)) { return Err(unavailable()); }
    let (pos, affiliation) = c.world.with(system, move |s| s.ball(ship).filter(|b| b.char_id == Some(owner) && b.mode != MODE_WARP && b.cloak == 0).map(|b| (b.position_at(s.now_ms()), (b.corp_id, b.alliance_id)))).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?.ok_or_else(|| notify_error(text::NOT_IN_SPACE))?;
    let mut fit = Fit::drone(&c.db, c.sde.clone(), owner, id).await?;
    let key = Key::Drone(id);
    crate::services::effects::fitting::required_skills(&fit, &c.sde, key, item.type_id, false)?;
    let max_active = fit.value(Key::Char, ATTR_MAX_ACTIVE_DRONES);
    let total_bandwidth = fit.value(Key::Ship, ATTR_DRONE_BANDWIDTH);
    check_count(count, max_active)?;
    let original_spec = super::super::worker::spec(&fit, key, item.type_id)?;
    let pristine_spec = if item.singleton {
        original_spec.clone()
    } else {
        fit.include_drone(id, item.type_id, Vec::new());
        crate::services::effects::fitting::required_skills(&fit, &c.sde, key, item.type_id, false)?;
        super::super::worker::spec(&fit, key, item.type_id)?
    };
    let mut original_fit = item.singleton.then_some(fit);
    let retains_original = i64::from(count) == item.quantity;
    let mut bandwidths = vec![pristine_spec.bandwidth; count as usize];
    if retains_original { bandwidths[0] = original_spec.bandwidth; }
    let token = c.world.with(system, move |s| s.reserve_drone_launch(owner, ship, &bandwidths, max_active, total_bandwidth)).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?.ok_or_else(|| notify_error(text::BANDWIDTH_EXCEEDED))?;
    let ship_projections = c.world.with(system, move |s| s.projections_on(ship)).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    let expected = item.clone();
    let world = c.world.clone();
    let effects = super::super::worker::effects(db.clone(), std::sync::Arc::downgrade(&world), system);
    let actor_world = world.clone();
    let (receipt, receiver) = tokio::sync::oneshot::channel();
    let submitted = actor_world.with(system, move |s| s.try_defer(async move {
    let committed = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            let expected = expected.clone();
            let ship_projections = ship_projections.clone();
            let mut original_fit = original_fit.take();
            Box::pin(async move {
                if !ship_dao::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(unavailable()); }
                if crate::dao::characters::selection::ship_id(tx, owner).await.map_err(db_err)? != Some(ship) { return Err(unavailable()); }
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
                if hull.owner_id != owner || hull.location_id != system || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton { return Err(unavailable()); }
                let fresh = load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
                if fresh.owner_id != owner || fresh.location_id != ship || fresh.flag_id != FLAG_DRONE_BAY || fresh.category_id != i64::from(CATEGORY_DRONE) || fresh.type_id != expected.type_id || fresh.quantity != expected.quantity || fresh.singleton != expected.singleton { return Err(unavailable()); }
                let ids = drones::launch(tx, &fresh, count, owner, ship, system, pos).await.map_err(db_err)?.ok_or_else(unavailable)?;
                let mut change = vec![(IX_STACK_SIZE, Value::Int(fresh.quantity))];
                if retains_original {
                    change.push((IX_LOCATION_ID, Value::Int(ship)));
                    change.push((IX_FLAG, Value::Int(i64::from(FLAG_DRONE_BAY))));
                    if !fresh.singleton { change.push((IX_SINGLETON, Value::Bool(false))); }
                }
                crate::services::inventory::notifications::notify_item_change(tx, &sde, outbox, id, change).await?;
                for &new_id in ids.iter().filter(|&&new_id| new_id != id) {
                    crate::services::inventory::notifications::notify_item_change(tx, &sde, outbox, new_id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                }
                let mut entries = Vec::with_capacity(ids.len());
                for &new_id in &ids {
                    let key = Key::Drone(new_id);
                    let fit = if new_id == id {
                        if let Some(mut fit) = original_fit.take() {
                            fit.replace_projections_for(Key::Ship, ship_projections.clone(), None);
                            fit
                        } else {
                            Fit::load_drone(tx, sde.clone(), owner, ship, new_id, ship_projections.clone(), Vec::new()).await?
                        }
                    } else {
                        Fit::load_drone(tx, sde.clone(), owner, ship, new_id, ship_projections.clone(), Vec::new()).await?
                    };
                    let spec = super::super::worker::spec(&fit, key, fresh.type_id)?;
                    let condition = crate::services::combat::prepare_condition_with_fit_key(tx, new_id, &fit, key).await?;
                    let ball = super::drone_ball(&fit, key, new_id, fresh.type_id, owner, affiliation, pos,
                        condition.fractions().map(|v| v.clamp(0.0, 1.0)))?;
                    entries.push(crate::space::drone::PreparedDrone { ball, spec, condition, fit, key });
                }
                Ok((ids, entries))
            })
        },
        |(ids, entries), outbox| {
            let world = &world;
            let market = &market;
            async move {
            let entries = std::sync::Arc::new(std::sync::Mutex::new(Some(entries)));
            let first = entries.clone();
            let first_effects = effects.clone();
            let published = world.with(system, move |s| {
                let entries = first.lock().unwrap().take().unwrap();
                s.complete_drone_launch(token, owner, ship, entries, first_effects)
            }).await;
            let ack = match published {
                Some(ack) => Some(ack),
                None => match world.system_locked(system, market).await {
                    Ok(()) => world.with(system, move |s| {
                        entries.lock().unwrap().take().map(|entries|
                            s.complete_drone_launch(token, owner, ship, entries, effects))
                    }).await.flatten(),
                    Err(error) => {
                        tracing::warn!(system, %error, "failed to restore committed drone launch");
                        None
                    }
                },
            };
            let admission = match ack {
                Some(Ok(ack)) => ack.await.map_err(|error| CallError::Internal(error.into())),
                Some(Err(error)) => Err(error),
                None => Err(CallError::Internal(anyhow::anyhow!("system actor stopped during committed launch restore"))),
            };
            if let Err(error) = dispatch_world((), outbox, &world) {
                tracing::warn!(system, ?error, "committed drone launch notification failed");
            }
            Ok(admission.map(|()| ids))
            }
        },
    ).await;
    let result = match committed {
        Ok(result) => result,
        Err(error) => {
            let _ = world.with(system, move |s| s.cancel_drone_launch(token)).await;
            Err(error)
        }
    };
    let _ = receipt.send(result);
    })).await.unwrap_or(false);
    if !submitted {
        let _ = c.world.with(system, move |s| s.cancel_drone_launch(token)).await;
        return Err(CallError::Internal(anyhow::anyhow!("drone launch worker stopped")));
    }
    receiver.await.map_err(|error| CallError::Internal(error.into()))?
}

#[cfg(test)] mod tests;
