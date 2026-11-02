//! Physical drone recovery and contact-time mining delivery.
//! Inventory transitions commit before space removal and client notifications.
use std::sync::Arc;
use marshal::Value;
use crate::client::Client;
use crate::consts::{dogma::ATTR_DRONE_CAPACITY, CATEGORY_DRONE, CATEGORY_SHIP, DRONE_SCOOP_RANGE, FLAG_DRONE_BAY, IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, OWNER_UNOWNED};
use crate::dao::items::{drones, loading::ItemQuery, ship as ship_dao};
use crate::db::{Db, executor::Executor, outbox::{dispatch_world, Outbox}};
use crate::messages::drones as text;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, Call, CallError, CallResult, fit::{Fit, Key}, items::load_items};
use crate::space::{dist, drone::{DroneOrder, DroneRecord}, World, MODE_WARP};

fn parse(args: &[Value]) -> Result<Vec<i64>, CallError> {
    let [Value::List(values)] = args else { return Err(notify_error(text::INVALID_COMMAND)) };
    let mut ids = Vec::with_capacity(values.len());
    for value in values {
        let Value::Int(id) = value else { return Err(notify_error(text::INVALID_COMMAND)) };
        if *id <= 0 { return Err(notify_error(text::INVALID_COMMAND)); }
        ids.push(*id);
    }
    ids.sort_unstable();
    if ids.is_empty() || ids.windows(2).any(|w| w[0] == w[1]) { return Err(notify_error(text::INVALID_COMMAND)); }
    Ok(ids)
}

#[derive(Clone)]
struct Source { id: i64, owner: i64, type_id: i32 }

async fn prepare_ship(ex: &(impl Executor + ?Sized), sde: &Sde, owner: i64, ship: i64, system: i64) -> Result<(), CallError> {
    let unavailable = || notify_error(text::NOT_AVAILABLE);
    if !ship_dao::lock_reload_ship(ex, ship).await.map_err(db_err)? || crate::dao::characters::selection::ship_id(ex, owner).await.map_err(db_err)? != Some(ship) { return Err(unavailable()); }
    let hull = load_items(ex, sde, ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next().ok_or_else(unavailable)?;
    if hull.owner_id != owner || hull.location_id != system || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton { return Err(unavailable()); }
    Ok(())
}

async fn lock_sources(ex: &(impl Executor + ?Sized), sde: &Sde, system: i64, sources: &[Source]) -> Result<(), CallError> {
    for source in sources {
        if !ship_dao::lock_reload_ship(ex, source.id).await.map_err(db_err)? { return Err(notify_error(text::NOT_AVAILABLE)); }
        let item = load_items(ex, sde, ItemQuery::Id(source.id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
        if item.owner_id != source.owner || item.location_id != system || item.flag_id != 0 || !item.singleton || item.quantity != 1 || item.category_id != i64::from(CATEGORY_DRONE) || item.type_id != source.type_id { return Err(notify_error(text::NOT_AVAILABLE)); }
        if !drones::positioned(ex, system, &[source.type_id]).await.map_err(db_err)?.iter().any(|row| row.item_id == source.id && row.owner_id == source.owner) { return Err(notify_error(text::NOT_AVAILABLE)); }
    }
    Ok(())
}

async fn check_bay(ex: &(impl Executor + ?Sized), sde: &Sde, ship: i64, capacity: f64, sources: &[Source]) -> Result<(), CallError> {
    let items = load_items(ex, sde, ItemQuery::LocationFlags(ship, &[FLAG_DRONE_BAY])).await.map_err(db_err)?;
    let raw = crate::dao::items::loading::load_items(ex, ItemQuery::LocationFlags(ship, &[FLAG_DRONE_BAY])).await.map_err(db_err)?;
    if items.len() != raw.len() { return Err(CallError::Internal(anyhow::anyhow!("bay item missing SDE type"))); }
    let mut used = 0.0;
    for (type_id, quantity) in items.iter().map(|i| (i.type_id, i.quantity)).chain(sources.iter().map(|i| (i.type_id, 1))) {
        let volume = sde.types.get(type_id).and_then(|t| t.volume).ok_or_else(|| CallError::Internal(anyhow::anyhow!("bay item missing volume")))?;
        if !volume.is_finite() || volume < 0.0 || quantity <= 0 { return Err(notify_error(text::NOT_AVAILABLE)); }
        used += volume * f64::from(quantity);
    }
    if !capacity.is_finite() || capacity < 0.0 || !used.is_finite() || used > capacity { return Err(notify_error(text::BAY_FULL)); }
    Ok(())
}

async fn move_sources(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, sde: &Sde, owner: i64, ship: i64, system: i64, sources: &[Source]) -> Result<(), CallError> {
    for source in sources {
        crate::services::combat::drone_health::save_condition_in(ex, source.id).await?;
        if !drones::return_to_bay(ex, source.id, source.owner, system, owner, ship).await.map_err(db_err)? { return Err(notify_error(text::NOT_AVAILABLE)); }
        let item = load_items(ex, sde, ItemQuery::Id(source.id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
        let changes = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(system)), (Value::Int(IX_FLAG), Value::Int(0)), (Value::Int(IX_OWNER_ID), Value::Int(source.owner))]);
        outbox.notify_pilot(owner, "OnItemChange", vec![item.row(), changes, Value::None]);
    }
    Ok(())
}

pub(crate) async fn scoop(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ids = parse(call.args)?;
    let (Some(owner), Some(ship), Some(system)) = (c.session.int("charid"), c.session.int("shipid"), c.session.int("solarsystemid")) else { return Err(notify_error(text::NOT_IN_SPACE)) };
    let bound = call.bind_params.and_then(Value::items);
    if !call.bound || c.session.int("stationid").is_some() || !bound.is_some_and(|v| v.len() == 2 && v[0].as_int() == Some(system) && v[1].as_int() == Some(5)) { return Err(notify_error(text::NOT_IN_SPACE)); }
    let _lock = crate::services::market::lock().await;
    let sde = c.sde.clone();
    let world = c.world.clone();
    let snapshot_ids = ids.clone();
    let sources = world.with(system, move |s| {
        let ship_ball = s.sampled_ball(ship).filter(|b| b.char_id == Some(owner) && b.mode != MODE_WARP && b.cloak == 0).ok_or_else(|| notify_error(text::NOT_IN_SPACE))?;
        let mut sources = Vec::with_capacity(snapshot_ids.len());
        for id in snapshot_ids {
            let ball = s.sampled_ball(id).ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
            if sde.types.get(ball.type_id).and_then(|t| t.category_id) != Some(CATEGORY_DRONE) { return Err(notify_error(text::NOT_AVAILABLE)); }
            if let Some(drone) = s.drone(id) {
                if drone.controller.is_some() || drone.owner != OWNER_UNOWNED { return Err(notify_error(text::NOT_CONTROLLED)); }
            }
            if dist(ball.pos, ship_ball.pos) - f64::from(ball.radius) - f64::from(ship_ball.radius) > DRONE_SCOOP_RANGE { return Err(notify_error(text::TARGET_OUT_OF_RANGE)); }
            sources.push(Source { id, owner: ball.owner_id, type_id: ball.type_id });
        }
        Ok(sources)
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))??;
    let fit = Fit::load_in_space(&c.db, &world, system, owner, ship).await?;
    let capacity = fit.value(Key::Ship, ATTR_DRONE_CAPACITY);
    let sde = c.sde.clone();
    let committed = c.db.transaction(|tx, outbox: &mut Outbox| {
        let sources = sources.clone(); let sde = sde.clone();
        Box::pin(async move {
            prepare_ship(tx, &sde, owner, ship, system).await?;
            lock_sources(tx, &sde, system, &sources).await?;
            check_bay(tx, &sde, ship, capacity, &sources).await?;
            move_sources(tx, outbox, &sde, owner, ship, system, &sources).await
        })
    }, |(), outbox| {
        let world = world.clone();
        async move {
            let task = tokio::spawn(async move {
                let fallback_ids = ids.clone();
                if world.with(system, move |s| { for id in ids { s.remove_ball(id); } }).await.is_none() {
                    match world.system(system).await {
                        Ok(()) => {
                            if world.with(system, move |s| { for id in fallback_ids { s.remove_ball(id); } }).await.is_none() {
                                tracing::warn!(system, "system actor stopped during committed recovery restore");
                            }
                        }
                        Err(error) => tracing::warn!(system, %error, "failed to restore committed drone recovery"),
                    }
                }
            });
            Ok((outbox, task))
        }
    }).await?;
    if let Err(error) = committed.1.await { tracing::warn!(system, %error, "committed drone recovery completion failed"); }
    if let Err(error) = dispatch_world((), committed.0, &world) {
        tracing::warn!(system, ?error, "committed drone recovery notification failed");
    }
    Ok(Value::None)
}

pub(crate) async fn contact(db: &Db, world: &Arc<World>, system: i64, drone: DroneRecord, return_to_bay: bool) -> Result<(), CallError> {
    let db = db.clone();
    let world = world.clone();
    tokio::spawn(async move {
        let id = drone.id; let generation = drone.generation;
        let Some(claimed) = world.with(system, move |s| s.drone_claim_contact(id, generation)).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))? else { return Ok(()) };
        let result = contact_claimed(&db, &world, system, &claimed, return_to_bay).await;
        if result.is_err() {
            world.with(system, move |s| { s.drone_idle(id, generation); s.drone_release_contact(id, generation); }).await;
        }
        result
    }).await.map_err(|error| CallError::Internal(error.into()))?
}

async fn contact_claimed(db: &Db, world: &Arc<World>, system: i64, drone: &DroneRecord, return_to_bay: bool) -> Result<(), CallError> {
    let id = drone.id; let generation = drone.generation; let owner = drone.owner;
    if return_to_bay != (drone.order == DroneOrder::ReturnBay) { return Err(notify_error(text::TRANSITION_BUSY)); }
    let ship = if return_to_bay { drone.target } else { drone.controller }.ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
    let _lock = crate::services::market::lock().await;
    let fit = Fit::load_in_space(db, world, system, owner, ship).await?;
    let capacity = fit.value(Key::Ship, ATTR_DRONE_CAPACITY);
    let destination = crate::services::mining::destination(&fit);
    let source = Source { id, owner, type_id: drone.spec.type_id };
    let pending = drone.pending.clone();
    let sde = world.sde().clone();
    let (outbox, task) = db.transaction(|tx, outbox: &mut Outbox| {
        let source = source.clone(); let pending = pending.clone(); let sde = sde.clone();
        Box::pin(async move {
            prepare_ship(tx, &sde, owner, ship, system).await?;
            lock_sources(tx, &sde, system, std::slice::from_ref(&source)).await?;
            if return_to_bay { check_bay(tx, &sde, ship, capacity, std::slice::from_ref(&source)).await?; }
            let delivery = if let Some(pending) = pending.as_ref() {
                let outcome = crate::services::mining::deliver(tx, outbox, &sde, owner, ship, system, id, pending, destination).await?;
                if let crate::services::mining::DeliveryOutcome::Full { flag } = outcome {
                    return Err(notify_error(if flag == crate::consts::FLAG_CARGO { crate::messages::CARGO_HOLD_FULL } else { crate::messages::MINING_HOLD_FULL }));
                }
                Some(outcome)
            } else { None };
            if return_to_bay { move_sources(tx, outbox, &sde, owner, ship, system, std::slice::from_ref(&source)).await?; }
            Ok(delivery)
        })
    }, |delivery, outbox| {
        let world = world.clone();
        let db = db.clone();
        async move {
            let task = tokio::spawn(async move {
                let repeat = match &delivery {
                    Some(crate::services::mining::DeliveryOutcome::Delivered(receipt)) => Some(receipt.repeat()),
                    Some(crate::services::mining::DeliveryOutcome::NoYield) => Some(true),
                    _ => None,
                };
                let completed = world.with(system, move |s| {
                    if return_to_bay {
                        s.remove_ball(id);
                    } else {
                        if let Some(repeat) = repeat { s.drone_delivered(id, generation, repeat); }
                        else { s.drone_idle(id, generation); }
                        s.drone_release_contact(id, generation);
                    }
                }).await;
                if completed.is_none() {
                    match world.system(system).await {
                        Ok(()) => {
                            if world.with(system, move |s| {
                                if return_to_bay { s.remove_ball(id); }
                                else {
                                    if let Some(repeat) = repeat { s.drone_delivered(id, generation, repeat); }
                                    else { s.drone_idle(id, generation); }
                                    s.drone_release_contact(id, generation);
                                }
                            }).await.is_none() {
                                tracing::warn!(system, "system actor stopped during committed contact restore");
                            }
                        }
                        Err(error) => tracing::warn!(system, %error, "failed to restore committed drone contact"),
                    }
                }
                match delivery {
                    Some(crate::services::mining::DeliveryOutcome::Delivered(receipt)) => {
                        if let Err(error) = crate::services::mining::delivered(&db, &world, system, owner, receipt).await {
                            tracing::warn!(system, ?error, "committed drone delivery completion failed");
                        }
                    }
                    Some(crate::services::mining::DeliveryOutcome::Depleted { rock }) => {
                        if let Err(error) = crate::services::mining::reconcile_depleted(&db, &world, rock).await {
                            tracing::warn!(system, ?error, "depleted rock reconciliation failed");
                        }
                    }
                    _ => {}
                }
            });
            Ok((outbox, task))
        }
    }).await?;
    if let Err(error) = task.await { tracing::warn!(system, %error, "committed drone contact task failed"); }
    if let Err(error) = dispatch_world((), outbox, world) {
        tracing::warn!(system, ?error, "committed drone contact notification failed");
    }
    Ok(())
}

#[cfg(test)] mod tests;
