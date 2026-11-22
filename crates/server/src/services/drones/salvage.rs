//! Salvage drone cycle ends and cargo transport contact.
//! One admitted end per brain is in flight at a time; it rolls the shared
//! chance against the wreck's SDE difficulty and settles through the shared
//! recovery transaction with the drone item as the reward destination. A
//! committed recovery marks the drone carried and the brain flies the cargo
//! home. An ambiguous settle outcome is reconciled once against the
//! operator's journaled wreck claim and the drone's real children; an
//! unconfirmed proof stays closed and busy rather than guessing a rollback.
//! On ship contact the real children move to the mother ship's cargo hold in
//! one transaction after that ship's pending loaded rows settle. A full hold
//! or any failed delivery is stop-and-hold: the cargo stays on the drone,
//! the owner is told once, and only the next player salvage, return or
//! scoop command retries. Mining-only contact keeps its existing path and is
//! never routed here.
use std::sync::Arc;
use marshal::Value;
use infinity_core::consts::FLAG_CARGO;
use infinity_core::messages;
use infinity_core::messages::drones as text;
use crate::dao::items::{loading::ItemQuery, parent_locks::LockStrength};
use crate::db::{Db, outbox::{dispatch_sender, Outbox, SourceKey}};
use crate::services::{db_err, market, notify_error, rand01, CallError,
    items::load_items,
    combat::loaded::{settle_in, LoadedHolds},
    effects::salvage::recovery::{self, RecoveryAuthority, RecoveryOutcome, RecoveryRequest, RewardDestination},
    inventory::{destination, hold_storage::HoldCapacity}};
use crate::space::{dist, Ball, NotifyTx, World, drone::DroneRecord};
use crate::lua::events::GameEvent;

/// The end-phase adapter: admit in the actor, roll once, then run the shared
/// durable settlement. A miss or an unavailable wreck resumes the ordinary
/// cycle; a recovered wreck starts the cargo transport.
pub(in crate::services) async fn finish_cycle(
    db: &Db, world: &Arc<World>, system: i64, drone: DroneRecord, wreck: Ball, range: f64, bonus: f64,
) -> Result<(), CallError> {
    let id = drone.id;
    let generation = drone.generation;
    let wreck_id = wreck.id;
    let resume = |world: &Arc<World>| {
        let world = world.clone();
        async move { let _ = world.with(system, move |s| s.drone_salvage_resume(id, generation)).await; }
    };
    let admitted = world.with(system, move |s| {
        let in_range = s.sampled_ball(id).zip(s.sampled_ball(wreck_id)).is_some_and(|(a, b)|
            (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) <= range);
        if !in_range { return None; }
        s.drone_salvage_admit(id, generation, wreck_id)
    }).await.flatten();
    let Some((membership, cancelled, sender)) = admitted else { return Ok(()) };
    let sde = world.sde().clone();
    let Some(ship) = drone.controller else {
        resume(world).await;
        return Ok(());
    };
    // A parked plan of this same order already holds a frozen success draw:
    // resume it instead of rolling chance or yields again.
    let attempt = crate::space::system::npc_wrecks::RecoveryAttempt::Drone { item: id, generation };
    let yields = match recovery::parked_yields(world, system, wreck_id, drone.owner, ship, id, id, range, attempt).await {
        Some(yields) => yields,
        None => {
            let Some(difficulty) = recovery::difficulty(&sde, wreck.type_id) else {
                resume(world).await;
                return Err(CallError::Internal(anyhow::anyhow!("wreck difficulty missing")));
            };
            if rand01() >= recovery::chance(bonus, difficulty) {
                resume(world).await;
                return Ok(());
            }
            match recovery::select_yields(&sde) {
                Ok(yields) => yields,
                Err(error) => { resume(world).await; return Err(error); }
            }
        }
    };
    let had_yields = !yields.is_empty();
    let request = RecoveryRequest {
        system,
        character: drone.owner,
        ship,
        wreck,
        range,
        authority: RecoveryAuthority::Drone { item: id, generation, membership, cancelled },
        destination: RewardDestination::Drone { item: id, owner: drone.owner, type_id: drone.spec.type_id },
        yields,
    };
    match recovery::settle(db, world, request, sender.clone()).await {
        Ok(RecoveryOutcome::Recovered) => {
            let _ = world.with(system, move |s| s.drone_salvage_carried(id, generation)).await;
            Ok(())
        }
        Ok(RecoveryOutcome::Unavailable) => {
            resume(world).await;
            Ok(())
        }
        Ok(RecoveryOutcome::Deferred { unknown: false, error }) => {
            // A proven refusal or a still-waiting parked plan: hold once and
            // tell the caller; only the next player command retries delivery.
            let _ = world.with(system, move |s| s.drone_salvage_blocked(id, generation)).await;
            if let Some(sender) = &sender { refuse(sender, &error); }
            Ok(())
        }
        Ok(RecoveryOutcome::Deferred { unknown: true, error }) => {
            // A possibly-executed attempt stayed unproven: close and hold the
            // order unknown; the sealed claim keeps the draw from re-rolling.
            let _ = world.with(system, move |s| s.drone_salvage_unknown(id, generation)).await;
            Err(error)
        }
        Err(error @ CallError::User { .. }) => {
            // A refused settlement rolled back for certain: hold once and tell
            // the caller; only the next player command retries delivery.
            let _ = world.with(system, move |s| s.drone_salvage_blocked(id, generation)).await;
            if let Some(sender) = &sender { refuse(sender, &error); }
            Ok(())
        }
        Err(error) => {
            // An ambiguous outcome is reconciled once against durable facts:
            // this operator's unique WreckSalvaged source and the real children
            // aboard this drone prove the commit together; a missing wreck row
            // could be another winner's claim and cargo could come from another
            // operation. Another operator's source proves this settle lost; an
            // absent event or missing expected cargo proves nothing, so the
            // outcome stays closed and busy and is never re-rolled.
            enum Proof { Committed, Lost, Unknown }
            let sde = world.sde().clone();
            let proof = async {
                let key = SourceKey::new("WreckSalvaged", wreck_id);
                let Some(envelope) = crate::dao::progress::source(db, "business", key.as_str()).await.map_err(db_err)?
                    else { return Ok(Proof::Unknown) };
                if !matches!(envelope.event, GameEvent::WreckSalvaged { char_id, .. } if char_id == drone.owner) {
                    return Ok(Proof::Lost);
                }
                let children = load_items(db, &sde, ItemQuery::Location(id)).await.map_err(db_err)?;
                // The real cargo corroborates the commit; a settle that planned
                // no yield is complete on the claim alone. Cargo missing while
                // yield was expected is durable nonsense, not proof either way.
                Ok::<Proof, CallError>(if children.is_empty() && had_yields { Proof::Unknown } else { Proof::Committed })
            }.await;
            match proof {
                Ok(Proof::Committed) => {
                    let _ = world.with(system, move |s| s.drone_salvage_carried(id, generation)).await;
                    Ok(())
                }
                Ok(Proof::Lost) => {
                    resume(world).await;
                    Ok(())
                }
                Ok(Proof::Unknown) | Err(_) => {
                    let _ = world.with(system, move |s| s.drone_salvage_unknown(id, generation)).await;
                    Err(error)
                }
            }
        }
    }
}

/// The contact adapter for a carried drone: the ordinary contact fence claims
/// it, one transaction settles the mother ship's pending loaded rows then
/// moves the real cargo children into its cargo hold.
pub(in crate::services::drones) async fn contact(
    db: &Db, world: &Arc<World>, system: i64, drone: DroneRecord,
) -> Result<(), CallError> {
    let db = db.clone();
    let world = world.clone();
    let id = drone.id;
    let generation = drone.generation;
    tokio::spawn(async move {
        let Some((claimed, sender)) = world.with(system, move |s| {
            s.drone_claim_contact(id, generation).map(|claimed| {
                let sender = s.pilot_sender(claimed.owner);
                (claimed, sender)
            })
        }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))? else { return Ok(()) };
        match transport_claimed(&db, &world, system, &claimed, sender.clone()).await {
            Ok(delivery) => {
                match delivery {
                    super::inventory::cargo::CargoDelivery::Empty | super::inventory::cargo::CargoDelivery::Delivered => {
                        let _ = world.with(system, move |s| s.drone_salvage_delivered(id, generation)).await;
                    }
                    super::inventory::cargo::CargoDelivery::Full => {
                        let _ = world.with(system, move |s| s.drone_salvage_blocked(id, generation)).await;
                        tell(&sender, messages::CARGO_HOLD_FULL);
                    }
                }
                Ok(())
            }
            Err(error) => {
                let _ = world.with(system, move |s| s.drone_salvage_blocked(id, generation)).await;
                match &error {
                    CallError::User { .. } => {
                        if let Some(sender) = &sender { refuse(sender, &error); }
                        Ok(())
                    }
                    CallError::Internal(_) => Err(error),
                }
            }
        }
    }).await.map_err(|error| CallError::Internal(error.into()))?
}

/// One business transaction: settle the ship's pending loaded rows, verify the
/// hull, lock the drone root, then deliver every actual child into the hold.
async fn transport_claimed(
    db: &Db, world: &Arc<World>, system: i64, drone: &DroneRecord, sender: Option<NotifyTx>,
) -> Result<super::inventory::cargo::CargoDelivery, CallError> {
    let id = drone.id;
    let generation = drone.generation;
    let owner = drone.owner;
    let ship = drone.controller.ok_or_else(|| notify_error(text::NOT_AVAILABLE))?;
    let busy = || notify_error(text::TRANSITION_BUSY);
    let armed = LoadedHolds::arm(world, &[(system, ship)], busy()).await?;
    let _market = market::lock().await;
    let mut holds = LoadedHolds::lock(world, armed, busy()).await?;
    let settle_rows = holds.settle_rows();
    let held_ships = holds.ships();
    // The cargo-hold snapshot and the final identity check both read the
    // resident actor: the resident fit supplies the real hold capacity.
    let capacity = world.with(system, move |s| {
        if !s.drone_owns_contact(id, generation) || !s.drone_carries_cargo(id) { return None; }
        let current = s.drone(id)?;
        if current.owner != owner || current.controller != Some(ship) || !s.drone_ship_live(owner, ship) { return None; }
        s.with_combat_fit(ship, |fit, _, _| HoldCapacity::capture(fit, FLAG_CARGO))
    }).await.flatten().transpose()?;
    let Some(capacity) = capacity else { return Err(notify_error(text::NOT_AVAILABLE)); };
    holds.may_execute();
    let source = super::inventory::cargo::CargoSource {
        drone: id, type_id: drone.spec.type_id, root_owner: owner, system,
    };
    let sde = world.sde().clone();
    let world_for_dispatch = world.clone();
    let result = db.transaction(
        move |tx, outbox: &mut Outbox| {
            let (sde, source, settle_rows) = (sde.clone(), source, settle_rows.clone());
            Box::pin(async move {
                settle_in(tx, &settle_rows).await?;
                super::inventory::prepare_ship(tx, &sde, owner, ship, system).await?;
                let requests = destination::parent_requests(&sde, [(source.drone, source.type_id)],
                    LockStrength::Update, LockStrength::Update)?;
                let locks = destination::lock_items(tx, &sde, &requests, &[]).await?;
                super::inventory::cargo::deliver_in(tx, outbox, &sde, &source, owner, ship, capacity, &locks).await
            })
        },
        move |value, outbox| {
            let sender = sender.clone();
            async move { dispatch_sender(value, outbox, &world_for_dispatch, sender.as_ref()) }
        },
    ).await;
    let seeds: Vec<_> = held_ships.into_iter().map(|ship| (ship, Vec::new())).collect();
    holds.finish(db, world, result, &seeds).await
}

/// Emit one OnRemoteMessage carrying `msg` and its dict entries to the pilot.
fn emit(sender: &NotifyTx, msg: &str, dict: Vec<(Value, Value)>) {
    let _ = sender.send(("OnRemoteMessage".into(), vec![Value::str(msg), Value::Dict(dict.into_iter().collect())]));
}

/// One CustomNotify remote message carrying the refusal text.
fn tell(sender: &Option<NotifyTx>, text: &str) {
    let Some(sender) = sender else { return };
    emit(sender, messages::MSG_CUSTOM_NOTIFY, vec![(Value::str("notify"), Value::unicode(text))]);
}

/// Forward a refused call's own message name and dict to the waiting pilot.
pub(in crate::services::drones) fn refuse(sender: &NotifyTx, error: &CallError) {
    let CallError::User { msg, dict } = error else { return };
    emit(sender, msg, dict.iter().map(|(key, value)| (Value::str(key), value.clone())).collect());
}
