//! Physical drone recovery and contact-time mining delivery.
//! Existing return/scoop departures reserve actor combat membership, checkpoint dirty conditions in the
//! transaction, then perform native removal from an owned deferred worker; mining-only contact keeps
//! its existing delivery/repeat policy.
use std::sync::Arc;
use marshal::Value;
use crate::client::Client;
use infinity_core::consts::{dogma::ATTR_DRONE_CAPACITY, CATEGORY_DRONE, CATEGORY_SHIP, DRONE_SCOOP_RANGE, FLAG_DRONE_BAY, IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, OWNER_UNOWNED};
use crate::dao::items::{drones, loading::ItemQuery, ship as ship_dao};
use crate::db::{Db, executor::Executor, outbox::{dispatch_world, Outbox}};
use infinity_core::messages::drones as text;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, Call, CallError, CallResult, fit::{Fit, Key}, items::load_items};
use crate::space::{dist, drone::DroneRecord, World, MODE_WARP};

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

#[derive(Clone, Copy)]
struct Source { id: i64, owner: i64, type_id: i32, generation: u64 }

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

async fn check_bay(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, ship: i64, capacity: f64, sources: &[Source]) -> Result<(), CallError> {
    let additions: Vec<_> = sources.iter().map(|source| crate::services::inventory::hold_storage::Addition {
        type_id: source.type_id, singleton: true, quantity: 1,
    }).collect();
    crate::services::inventory::hold_storage::check_destination(ex, sde, owner, ship, FLAG_DRONE_BAY, &additions, Default::default(), Some(capacity)).await.map(|_| ())
}

async fn move_sources(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, sde: &Sde, owner: i64, ship: i64, system: i64, sources: &[Source]) -> Result<(), CallError> {
    for source in sources {
        if !drones::return_to_bay(ex, source.id, source.owner, system, owner, ship).await.map_err(db_err)? { return Err(notify_error(text::NOT_AVAILABLE)); }
        let item = load_items(ex, sde, ItemQuery::Id(source.id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
        let changes = Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(system)), (Value::Int(IX_FLAG), Value::Int(0)), (Value::Int(IX_OWNER_ID), Value::Int(source.owner))]);
        outbox.notify_pilot(owner, "OnItemChange", vec![item.row(), changes, Value::None]);
    }
    Ok(())
}


fn transition_snapshots(transition: &crate::space::system::boarding::CombatTransition) -> Vec<crate::space::combat::ActorConditionSnapshot> {
    transition.entries.iter().map(|(_, snapshot)| snapshot.clone()).collect()
}

async fn reconcile_sources(db: &Db, sde: Arc<Sde>, owner: i64, ship: i64, system: i64,
    sources: Arc<[Source]>, facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>) -> Result<(Option<bool>, std::collections::HashSet<i64>), CallError> {
    db.transaction(move |tx, _: &mut ()| {
        let sde = sde.clone();
        let sources = sources.clone();
        let facts = facts.clone();
        Box::pin(async move {
        crate::dao::characters::selection::lock_character(tx, owner).await.map_err(db_err)?;
        ship_dao::lock_reload_ship(tx, ship).await.map_err(db_err)?;
        for source in sources.iter() {
            ship_dao::lock_reload_ship(tx, source.id).await.map_err(db_err)?;
        }
        let mut committed = true;
        let mut unchanged = true;
        for source in sources.iter() {
            let item = load_items(tx, &sde, ItemQuery::Id(source.id)).await.map_err(db_err)?.into_iter().next();
            committed &= item.as_ref().is_some_and(|item| item.location_id == ship && item.owner_id == owner && item.flag_id == FLAG_DRONE_BAY);
            unchanged &= item.as_ref().is_some_and(|item| item.location_id == system && item.owner_id == source.owner && item.flag_id == 0);
        }
        let receipt = match &facts { Some(frame) => frame.is_committed(tx).await.map_err(db_err)?, None => true };
        let written = if committed && receipt {
            match &facts { Some(frame) => frame.verified_condition_ids(tx).await.map_err(db_err)?, None => Default::default() }
        } else { Default::default() };
        Ok((if committed && receipt { Some(true) } else if unchanged { Some(false) } else { None }, written))
    }) }, |result, ()| async move { Ok(result) }).await
}

pub(crate) async fn scoop(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ids = parse(call.args)?;
    let (Some(owner), Some(ship), Some(system)) = (c.session.int("charid"), c.session.int("shipid"), c.session.int("solarsystemid")) else { return Err(notify_error(text::NOT_IN_SPACE)) };
    let bound = call.bind_params.and_then(Value::items);
    if !call.bound || c.session.int("stationid").is_some() || !bound.is_some_and(|v| v.len() == 2 && v[0].as_int() == Some(system) && v[1].as_int() == Some(5)) { return Err(notify_error(text::NOT_IN_SPACE)); }
    let world = c.world.clone();
    let admission_ids = ids.clone();
    let market = crate::services::market::lock().await;
    let gate = world.lock_condition_snapshot(system).await.map_err(|error| CallError::Internal(error.into()))?;
    let fit = Fit::load_in_space(&c.db, &world, system, owner, ship).await?;
    let capacity = fit.value(Key::Ship, ATTR_DRONE_CAPACITY);
    let sde = c.sde.clone();
    let (sources, transition) = world.with(system, move |s| {
        let ship_ball = s.sampled_ball(ship).filter(|b| b.char_id == Some(owner) && b.mode != MODE_WARP && b.cloak == 0).ok_or_else(|| notify_error(text::NOT_IN_SPACE))?;
        let mut sources = Vec::with_capacity(admission_ids.len());
        for id in admission_ids.iter().copied() {
            let ball = s.sampled_ball(id).ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
            if sde.types.get(ball.type_id).and_then(|t| t.category_id) != Some(CATEGORY_DRONE) { return Err(notify_error(text::NOT_AVAILABLE)); }
            if let Some(drone) = s.drone(id) {
                if drone.controller.is_some() || drone.owner != OWNER_UNOWNED { return Err(notify_error(text::NOT_CONTROLLED)); }
            }
            if dist(ball.pos, ship_ball.pos) - f64::from(ball.radius) - f64::from(ship_ball.radius) > DRONE_SCOOP_RANGE { return Err(notify_error(text::TARGET_OUT_OF_RANGE)); }
            sources.push(Source { id, owner: ball.owner_id, type_id: ball.type_id, generation: s.persistent_generation(id).ok_or_else(|| notify_error(text::TRANSITION_BUSY))? });
        }
        let transition = s.begin_condition_transitions(&admission_ids).ok_or_else(|| notify_error(text::TRANSITION_BUSY))?;
        Ok((sources, transition))
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))??;
    let sources: Arc<[Source]> = sources.into();
    let transition_entries = transition.entries.clone();
    let snapshots = transition_snapshots(&transition);
    let facts = transition.facts.clone();
    let ids_for_job = ids.clone();
    let transition_entries_for_failure = transition_entries.clone();
    let db = c.db.clone(); let sde = c.sde.clone(); let world_for_job = world.clone();
    let (receipt, receiver) = tokio::sync::oneshot::channel();
    let submitted = world.with(system, move |s| s.defer_condition_transition(transition, move |_transition| async move {
        let _market = market;
        let _gate = gate;
        let result = db.transaction(|tx, outbox: &mut Outbox| {
            let sources = sources.clone(); let sde = sde.clone(); let snapshots = snapshots.clone();
            let facts = facts.clone();
            Box::pin(async move {
                prepare_ship(tx, &sde, owner, ship, system).await?;
                lock_sources(tx, &sde, system, &sources).await?;
                check_bay(tx, &sde, owner, ship, capacity, &sources).await?;
                let written = crate::services::combat::checkpoint_conditions_in(tx, outbox, system, &snapshots, facts.as_deref()).await?;
                if facts.as_ref().is_some_and(|frame| frame.condition_rows().any(|row| !written.contains(&row.0))) {
                    return Err(CallError::Internal(anyhow::anyhow!("collateral condition was not checkpointed")));
                }
                if snapshots.iter().any(|snapshot| snapshot.revision != snapshot.saved_revision && !written.contains(&snapshot.item_id)) { return Err(CallError::Internal(anyhow::anyhow!("drone condition was not checkpointed"))); }
                move_sources(tx, outbox, &sde, owner, ship, system, &sources).await?;
                Ok(written)
            })
        }, |written, outbox| {
            let world_for_job = &world_for_job;
            let sources = sources.clone();
            let ids_for_job = ids_for_job.clone();
            let transition_entries = transition_entries.clone();
            let facts = facts.clone();
            async move {
                if let Some(facts) = facts {
                    let written = written.into_iter().collect();
                    let _ = world_for_job.with(system, move |s| facts.acknowledge(s, &written)).await;
                }
                let entries = transition_entries.clone();
                let removed = world_for_job.with(system, move |s| {
                    let valid = ids_for_job.iter().zip(entries.iter()).all(|(id, (token, snapshot))|
                        s.persistent_generation(*id) == sources.iter().find(|source| source.id == *id).map(|source| source.generation)
                            && s.combat_membership_generation(*id) == Some(snapshot.generation)
                            && matches!(s.combat_phase(*id), Some(crate::space::combat::CombatPhase::Transition { token: current }) if current == *token));
                    if !valid { return false; }
                    let mut removed = true;
                    for (id, (token, _snapshot)) in ids_for_job.iter().zip(entries.iter()) {
                        removed &= s.commit_combat_transition(*id, *token) && s.remove_ball(*id).is_some();
                    }
                    removed
                }).await.unwrap_or(false);
                if removed { if let Err(error) = dispatch_world((), outbox, world_for_job) { tracing::warn!(system, ?error, "committed drone scoop notification failed"); } Ok(Ok(())) }
                else { Ok(Err(CallError::Internal(anyhow::anyhow!("system actor stopped during committed drone scoop")))) }
            }
        }).await.and_then(|result| result);
        drop(_gate);
        drop(_market);
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let weak_world = Arc::downgrade(&world_for_job);
                let reconcile_db = db.clone();
                let reconcile_sde = sde.clone();
                let sources_for_reconcile = sources.clone();
                let entries = transition_entries.clone();
                let reconcile_facts = facts.clone();
                let callback: Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> = Arc::new(move || {
                    let Some(world) = weak_world.upgrade() else { return Box::pin(async {}) };
                    let db = reconcile_db.clone();
                    let sde = reconcile_sde.clone();
                    let sources = sources_for_reconcile.clone();
                    let entries = entries.clone();
                    let facts = reconcile_facts.clone();
                    Box::pin(async move {
                        let _market = crate::services::market::lock().await;
                        let _gate = match world.lock_condition_snapshot(system).await {
                            Ok(gate) => gate,
                            Err(_) => {
                                for (token, snapshot) in &entries {
                                    let item_id = snapshot.item_id; let generation = snapshot.generation; let token = *token;
                                    let _ = world.with(system, move |s| s.release_pending_combat(item_id, token, generation)).await;
                                }
                                return;
                            }
                        };
                        let (committed, written) = match reconcile_sources(&db, sde, owner, ship, system, sources, facts.clone()).await {
                            Ok((Some(value), written)) => (value, written),
                            _ => {
                                for (token, snapshot) in &entries {
                                    let item_id = snapshot.item_id; let generation = snapshot.generation; let token = *token;
                                    let _ = world.with(system, move |s| s.release_pending_combat(item_id, token, generation)).await;
                                }
                                return;
                            }
                        };
                        let _ = world.with(system, move |s| {
                            if committed {
                                if let Some(facts) = facts {
                                    facts.acknowledge(s, &written);
                                }
                            }
                            for (token, snapshot) in entries {
                                if committed {
                                    if s.persistent_generation(snapshot.item_id).is_some()
                                        && s.combat_membership_generation(snapshot.item_id) == Some(snapshot.generation)
                                        && s.commit_combat_transition(snapshot.item_id, token) { s.remove_ball(snapshot.item_id); }
                                } else {
                                    s.cancel_combat_transition(snapshot.item_id, token, snapshot.generation);
                                }
                            }
                        }).await;
                    })
                });
                let initial_callback = Arc::clone(&callback);
                let entries = transition_entries.clone();
                let pending = world_for_job.with(system, move |s| {
                    let mut pending = false;
                    for (token, snapshot) in entries {
                        pending |= s.mark_pending_transition(snapshot.item_id, token, snapshot.generation, Arc::clone(&callback));
                    }
                    pending
                }).await.unwrap_or(false);
                if pending {
                    initial_callback().await;
                } else {
                    let _ = world_for_job.with(system, move |s| {
                        for (token, snapshot) in transition_entries {
                            s.release_pending_combat(snapshot.item_id, token, snapshot.generation);
                        }
                    }).await;
                }
                Err(error)
            }
        };
        let _ = receipt.send(result);
    })).await.unwrap_or(false);
    if !submitted {
        let _ = world.with(system, move |s| {
            for (token, snapshot) in transition_entries_for_failure {
                s.cancel_combat_transition(snapshot.item_id, token, snapshot.generation);
            }
        }).await;
        return Err(CallError::Internal(anyhow::anyhow!("drone scoop worker stopped")));
    }
    receiver.await.map_err(|error| CallError::Internal(error.into()))??;
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
            world.with(system, move |s| {
                if matches!(s.combat_phase(id), Some(crate::space::combat::CombatPhase::Live)) { s.drone_idle(id, generation); }
                s.drone_release_contact(id, generation);
            }).await;
        }
        result
    }).await.map_err(|error| CallError::Internal(error.into()))?
}

async fn contact_return_claimed(
    db: &Db,
    world: &Arc<World>,
    system: i64,
    drone: &DroneRecord,
    ship: i64,
    fit: Fit,
    transition: crate::space::system::boarding::CombatTransition,
    market: tokio::sync::MutexGuard<'static, ()>,
    gate: tokio::sync::OwnedMutexGuard<()>,
) -> Result<(), CallError> {
    let id = drone.id;
    let facts = transition.facts.clone();
    let combat_generation = transition.entries[0].1.generation;
    let owner = drone.owner;
    let capacity = fit.value(Key::Ship, ATTR_DRONE_CAPACITY);
    let pending = drone.pending.clone();
    let destination = pending.as_ref().map(|pending| crate::services::mining::destination(&fit, pending.type_id));
    let persistent_generation = world.with(system, move |s| s.persistent_generation(id)).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("drone generation unavailable")))?;
    let source = Source { id, owner, type_id: drone.spec.type_id, generation: persistent_generation };
    let snapshots = transition_snapshots(&transition);
    let token = transition.entries[0].0;
    let sde = world.sde().clone();
    let snapshots = Arc::new(snapshots);
    let pending = Arc::new(pending);
    let db_for_job = db.clone();
    let submit_world = world.clone();
    let world_for_job = world.clone();
    let (receipt, receiver) = tokio::sync::oneshot::channel();
    let submitted = submit_world.with(system, move |s| s.defer_condition_transition(transition, move |_transition| async move {
        let _market = market;
        let _gate = gate;
        let result = db_for_job.transaction(|tx, outbox: &mut Outbox| {
            let sde = sde.clone(); let snapshots = snapshots.clone(); let pending = pending.clone();
            let facts = facts.clone();
            Box::pin(async move {
                prepare_ship(tx, &sde, owner, ship, system).await?;
                lock_sources(tx, &sde, system, std::slice::from_ref(&source)).await?;
                check_bay(tx, &sde, owner, ship, capacity, std::slice::from_ref(&source)).await?;
                let written = crate::services::combat::checkpoint_conditions_in(tx, outbox, system, &snapshots, facts.as_deref()).await?;
                if facts.as_ref().is_some_and(|frame| frame.condition_rows().any(|row| !written.contains(&row.0))) {
                    return Err(CallError::Internal(anyhow::anyhow!("collateral condition was not checkpointed")));
                }
                if snapshots.iter().any(|snapshot| snapshot.revision != snapshot.saved_revision && !written.contains(&snapshot.item_id)) {
                    return Err(CallError::Internal(anyhow::anyhow!("drone condition was not checkpointed")));
                }
                let delivery = if let Some(pending) = pending.as_ref() {
                    let outcome = crate::services::mining::deliver(tx, outbox, &sde, owner, ship, system, id, pending, destination.expect("pending mining has a destination")).await?;
                    if let crate::services::mining::DeliveryOutcome::Full { flag } = outcome {
                        return Err(notify_error(if flag == infinity_core::consts::FLAG_CARGO { infinity_core::messages::CARGO_HOLD_FULL } else { infinity_core::messages::MINING_HOLD_FULL }));
                    }
                    Some(outcome)
                } else { None };
                move_sources(tx, outbox, &sde, owner, ship, system, std::slice::from_ref(&source)).await?;
                Ok((delivery, written))
            })
        }, |(delivery, written), outbox| {
            let world_for_job = &world_for_job;
            let db_for_job = &db_for_job;
            let facts = facts.clone();
            async move {
                if let Some(facts) = facts {
                    let written = written.into_iter().collect();
                    let _ = world_for_job.with(system, move |s| facts.acknowledge(s, &written)).await;
                }
                let removed = world_for_job.with(system, move |s| {
                    s.persistent_generation(id) == Some(persistent_generation)
                        && s.combat_membership_generation(id) == Some(combat_generation)
                        && s.commit_combat_transition(id, token)
                        && s.remove_ball(id).is_some()
                }).await.unwrap_or(false);
                let result = if !removed {
                    Err(CallError::Internal(anyhow::anyhow!("system actor stopped during committed drone return")))
                } else {
                    if let Err(error) = dispatch_world((), outbox, world_for_job) { tracing::warn!(system, ?error, "committed drone return notification failed"); }
                    if let Some(delivery) = delivery {
                        match delivery {
                            crate::services::mining::DeliveryOutcome::Delivered(receipt) => {
                                if let Err(error) = crate::services::mining::delivered(&db_for_job, &world_for_job, system, owner, receipt).await { tracing::warn!(system, ?error, "committed drone delivery completion failed"); }
                            }
                            crate::services::mining::DeliveryOutcome::Depleted { rock } => {
                                if let Err(error) = crate::services::mining::reconcile_depleted(&db_for_job, &world_for_job, system, rock).await { tracing::warn!(system, ?error, "depleted rock reconciliation failed"); }
                            }
                            crate::services::mining::DeliveryOutcome::Missing => {}
                            crate::services::mining::DeliveryOutcome::Full { .. } => {}
                            crate::services::mining::DeliveryOutcome::NoYield => {}
                        }
                    }
                    Ok(())
                };
                Ok(result)
            }
        }).await.and_then(|result| result);
        drop(_gate);
        drop(_market);
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let sde = world_for_job.sde().clone();
                let weak_world = Arc::downgrade(&world_for_job);
                let reconcile_db = db_for_job.clone();
                let reconcile_facts = facts.clone();
                let callback: Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> = Arc::new(move || {
                    let Some(world) = weak_world.upgrade() else { return Box::pin(async {}) };
                    let db = reconcile_db.clone();
                    let sde = sde.clone();
                    let facts = reconcile_facts.clone();
                    Box::pin(async move {
                        let _market = crate::services::market::lock().await;
                        let _gate = match world.lock_condition_snapshot(system).await {
                            Ok(gate) => gate,
                            Err(error) => {
                                tracing::warn!(system, ?error, "drone return reconciliation gate unavailable");
                                let _ = world.with(system, move |s| s.release_pending_combat(id, token, combat_generation)).await;
                                return;
                            }
                        };
                        let (committed, written) = match reconcile_sources(&db, sde, owner, ship, system, Arc::from([source]), facts.clone()).await {
                            Ok((Some(value), written)) => (value, written),
                            _ => {
                                let _ = world.with(system, move |s| s.release_pending_combat(id, token, combat_generation)).await;
                                return;
                            }
                        };
                        let _ = world.with(system, move |s| {
                            if committed {
                                if let Some(facts) = facts {
                                    facts.acknowledge(s, &written);
                                }
                            }
                            if committed {
                                if s.persistent_generation(id) == Some(persistent_generation)
                                    && s.combat_membership_generation(id) == Some(combat_generation)
                                    && s.commit_combat_transition(id, token) { s.remove_ball(id); }
                            } else { s.cancel_combat_transition(id, token, combat_generation); }
                        }).await;
                    })
                });
                let initial_callback = Arc::clone(&callback);
                let pending = world_for_job.with(system, move |s| s.mark_pending_transition(id, token, combat_generation, callback)).await.unwrap_or(false);
                if pending {
                    initial_callback().await;
                }
                Err(error)
            }
        };
        let _ = receipt.send(result);
    })).await.unwrap_or(false);
    if !submitted { return Err(CallError::Internal(anyhow::anyhow!("drone return worker stopped"))); }
    receiver.await.map_err(|error| CallError::Internal(error.into()))?
}

async fn contact_claimed(db: &Db, world: &Arc<World>, system: i64, drone: &DroneRecord, return_to_bay: bool) -> Result<(), CallError> {
    let id = drone.id; let generation = drone.generation;
    let (drone, transition, preflight_fit, market, gate) = if return_to_bay {
        let market = crate::services::market::lock().await;
        let gate = world.lock_condition_snapshot(system).await.map_err(|error| CallError::Internal(error.into()))?;
        let target = drone.target.ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
        let fit = Fit::load_in_space(db, world, system, drone.owner, target).await?;
        let transition = world.with(system, move |s| {
            if !s.drone_owns_contact(id, generation) { return None; }
            s.begin_condition_transitions(&[id])
        }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?
            .ok_or_else(|| notify_error(text::TRANSITION_BUSY))?;
        (drone.clone(), Some(transition), Some(fit), Some(market), Some(gate))
    } else { (drone.clone(), None, None, None, None) };
    let owner = drone.owner;
    let ship = if return_to_bay { drone.target } else { drone.controller }.ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
    if let Some(transition) = transition {
        return contact_return_claimed(db, world, system, &drone, ship, preflight_fit.expect("return preflight fit"), transition,
            market.expect("return market guard"), gate.expect("return snapshot gate")).await;
    }

    let fit = Fit::load_in_space(db, world, system, owner, ship).await?;
    let capacity = fit.value(Key::Ship, ATTR_DRONE_CAPACITY);
    let source = Source { id, owner, type_id: drone.spec.type_id, generation: world.with(system, move |s| s.persistent_generation(id)).await.flatten().ok_or_else(|| notify_error(text::NOT_AVAILABLE))? };
    let pending = drone.pending.clone();
    let destination = pending.as_ref().map(|pending| crate::services::mining::destination(&fit, pending.type_id));
    let sde = world.sde().clone();
    let market = crate::services::market::lock().await;
    db.transaction(|tx, outbox: &mut Outbox| {
        let source = source.clone(); let pending = pending.clone(); let sde = sde.clone();
        Box::pin(async move {
            prepare_ship(tx, &sde, owner, ship, system).await?;
            lock_sources(tx, &sde, system, std::slice::from_ref(&source)).await?;
            if return_to_bay { check_bay(tx, &sde, owner, ship, capacity, std::slice::from_ref(&source)).await?; }
            let delivery = if let Some(pending) = pending.as_ref() {
                let outcome = crate::services::mining::deliver(tx, outbox, &sde, owner, ship, system, id, pending, destination.expect("pending mining has a destination")).await?;
                if let crate::services::mining::DeliveryOutcome::Full { flag } = outcome {
                    return Err(notify_error(if flag == infinity_core::consts::FLAG_CARGO { infinity_core::messages::CARGO_HOLD_FULL } else { infinity_core::messages::MINING_HOLD_FULL }));
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
                    match world.system_locked(system, &market).await {
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
                        if let Err(error) = crate::services::mining::reconcile_depleted(&db, &world, system, rock).await {
                            tracing::warn!(system, ?error, "depleted rock reconciliation failed");
                        }
                    }
                    _ => {}
                }
                if let Err(error) = dispatch_world((), outbox, &world) {
                    tracing::warn!(system, ?error, "committed drone contact notification failed");
                }
                drop(market);
            });
            if let Err(error) = task.await { tracing::warn!(system, %error, "committed drone contact task failed"); }
            Ok(())
        }
    }).await?;
    Ok(())
}

#[cfg(test)] mod tests;
