//! Mining: asteroid belts, ore/ice/gas sites' fields, mining lasers, strip miners, ice and
//! gas harvesters, the ship mining scanner and reprocessing at stations.
//!
//! Every asteroid belt of a system gets a field of rocks the first time a pilot enters
//! the system: ores by the system's security band, sized in the thousands of m3 each.
//! Exploration's ore, ice and gas sites get fields of their own (services/exploration).
//! Rocks are stored in game.asteroids; each World owns its system actor's committed
//! projection. Depletion and regrowth deadlines are durable and recovered on system load.
//!
//! A mining module runs through combat's activation loop (the `miningLaser` /
//! `miningClouds` effects): at the end of each cycle `extract` moves its yield
//! (miningAmount m3, with critical successes and residue waste) from the rock to
//! an accepting specialized hold, or cargo when no suitable hold exists, and tells the client
//! (OnOreMined -> notifications.oreMinedNotificationAdapter, OnItemChange).
//! Stack quantities sent on the wire project to the client display limit;
//! stored amounts stay exact.
//! Hold occupancy follows client item-aware packaged and assembled volumes.
//! Extraction range uses ship and rock positions sampled at the same system-actor time.
//! Laser extraction revalidates the online fitted module under row locks. Per-cycle
//! rounding is randomized as in `packages/mining/__init__.py`, with no fraction carry.
//! Attribute 3154 is percentage points, compared as in `mining/__init__.py:54`.
//! Completed cycles use clamp_factor=1 and server efficiency=1; partial cycles are
//! not delivered. Critical and waste consumption retain server-policy caps, which the
//! client shared formula does not specify. An owned task completes committed depletion
//! updates even if the initiating cycle is cancelled.
//! Each delivered extraction appends a personal fact to game.character_mining_ledger in
//! the caller's transaction, before post-commit notices. The client refreshes its
//! personal ledger normally; corporate observers are unsupported.
//!
//! Reprocessing (the station's bound reprocessingSvc) turns items into their
//! canonical SDE type materials per portion: the configured station yield, raised by Reprocessing,
//! Reprocessing Efficiency and the item's own processing skill, less the configured ISK tax.
//! Each output type is limited to one request's maximum quantity; deposits may span stacks.

mod in_space_compression_mgr;
mod ledger;
pub(crate) mod retention;
pub(crate) mod progress;
mod mining_scan_mgr;
mod reprocessing_svc;
pub(crate) mod rounding;
use super::fit::{Fit, Key};
use super::{db_err, keyval, notify_error, rand01, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use infinity_core::consts::{dogma::ATTR_MAX_RANGE, FILETIME_SECOND, FLAG_CARGO, FLAG_HANGAR, GROUP_ASTEROID_BELT, IX_LOCATION_ID, IX_STACK_SIZE};
use crate::db::Db;
use infinity_core::messages;
use crate::space::{dist, NotifyTx, World, V3};
use crate::space::asteroids::{self, Field};
use crate::space::asteroids::generation::{field_groups, generation_inputs};
use marshal::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tracing::{info, warn};

const ATTR_MINING_AMOUNT: i32 = 77;
const ATTR_WASTE_MULTIPLIER: i32 = 3153;
const ATTR_WASTE_PROBABILITY: i32 = 3154;
const ATTR_CRIT_CHANCE: i32 = 5967;
const ATTR_CRIT_BONUS: i32 = 5969;
const ATTR_PROCESSING_SKILL: i32 = 790; // reprocessingSkillType
const SKILL_REPROCESSING: i32 = 3385;
const SKILL_REPROCESSING_EFFICIENCY: i32 = 3389;
const SKILL_SCRAPMETAL: i32 = 12196;
/// Inclusive waste check maximum from `mining/__init__.py:54`.
const WASTE_CHECK_MAX: u32 = 100;


// ---------------------------------------------------------------- fields

/// Grow a field transactionally and publish it to the original system actor.
pub(crate) async fn grow_field(db: &Db, world: &World, system: i64, field_id: i64, center: V3, field: Field, security: f64, jspace: bool) -> Result<Vec<i64>, CallError> {
    world.system(system).await.map_err(CallError::Internal)?;
    let actor = world.asteroid_actor(system).ok_or_else(|| CallError::Internal(anyhow::anyhow!("asteroid actor unavailable")))?;
    let inputs = Arc::new(generation_inputs(world.sde(), center, field, security, jspace));
    let now = filetime_now();
    let _market = super::market::lock().await;
    let prepared = db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
        |tx, _| {
            let inputs = Arc::clone(&inputs);
            Box::pin(async move { asteroids::prepare_field(tx, system, field_id, now, &inputs, false).await })
        },
        |batch, _| async move { Ok(batch) },
    ).await?;
    let ids = prepared.ids();
    if !asteroids::publish(world, system, &actor, prepared).await {
        return Err(CallError::Internal(anyhow::anyhow!("asteroid field publication failed")));
    }
    Ok(ids)
}

/// Reconcile a committed zero row and its durable belt deadline with its original actor.
pub(crate) async fn reconcile_depleted(db: &Db, world: &Arc<World>, system: i64, id: i64) -> Result<(), CallError> {
    let Some(actor) = world.asteroid_actor(system) else { return Ok(()) };
    let Some(field) = actor.rock_field(id).await else { return Ok(()) };
    let belt = world.sde().map.celestial(field).filter(|b| b.group_id == Some(GROUP_ASTEROID_BELT as i64));
    let center = belt.map(|b| [b.x.expect("belt x"), b.y.expect("belt y"), b.z.expect("belt z")]);
    let regrowth = belt.map(|_| crate::config::game().mining.belt_regrowth_secs.saturating_mul(FILETIME_SECOND));
    let now = filetime_now();
    let result = db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
        |tx, _| Box::pin(async move { crate::dao::asteroids::deplete_scoped(tx, system, field, id, now, regrowth).await.map_err(db_err) }),
        |result, _| async move { Ok(result) },
    ).await?;
    match result {
        crate::dao::asteroids::Depletion::Deleted { field, deadline }
        | crate::dao::asteroids::Depletion::AlreadyDepleted { field, deadline } => {
            if !asteroids::publish_depletion(world, system, &actor, id, field, deadline, center).await {
                return Err(CallError::Internal(anyhow::anyhow!("asteroid depletion publication failed")));
            }
        }
        crate::dao::asteroids::Depletion::AbsentWrongField => {}
    }
    Ok(())
}

// ---------------------------------------------------------------- extraction


fn extraction_amounts(units: i64, waste: i64, available: i64, room: i64) -> (i64, i64) {
    let taken = units.max(0).min(available.max(0)).min(room.max(0));
    let wasted = if taken == 0 { 0 } else { waste.max(0).min(available - taken) };
    (taken, wasted)
}

/// Capture fitted values before entering an actor turn.
pub(crate) fn yield_values(fit: &Fit, key: Key) -> [f64; 5] {
    [fit.value(key, ATTR_MINING_AMOUNT).max(0.0), fit.value(key, ATTR_CRIT_CHANCE),
     fit.value(key, ATTR_CRIT_BONUS), fit.value(key, ATTR_WASTE_PROBABILITY), fit.value(key, ATTR_WASTE_MULTIPLIER)]
}

/// A source's fitted yield is an intent until a transaction commits it.
pub(crate) fn yield_intent(values: [f64; 5], target: i64, type_id: i32, unit_volume: f64) -> crate::space::drone::PendingMining {
    let [volume, crit_probability, crit_quantity_multiplier, waste_probability, waste_volume_multiplier] = values;
    crate::space::drone::PendingMining {
        rock: target, type_id, unit_volume,
        inputs: rounding::MiningInputs { clamp_factor: 1.0, volume, waste_volume_multiplier, waste_probability, crit_quantity_multiplier, crit_probability, efficiency: 1.0 },
        draws: rounding::MiningDraws {
            waste_check: (rand01() * f64::from(WASTE_CHECK_MAX + 1)) as u32,
            normal_round: rand01(), waste_round: rand01(), crit_check: rand01(), crit_round: rand01(),
        },
    }
}


pub(crate) struct Delivery {
    rock: i64,
    left: i64,
    removed: i64,
    full: bool,
}

impl Delivery {
    pub(crate) fn repeat(&self) -> bool { self.left > 0 && !self.full }
}

pub(crate) enum DeliveryOutcome {
    Missing,
    Depleted { rock: i64 },
    NoYield,
    Full { flag: i32 },
    Delivered(Delivery),
}

pub(crate) fn destination(fit: &Fit, type_id: i32) -> (i32, f64) {
    let flag = super::inventory::holds::mining_flag(fit.sde(), fit, type_id);
    (flag, super::inventory::holds::effective_capacity(fit, flag))
}

/// Join the caller's transaction; all notices remain in its post-commit outbox.
pub(crate) async fn deliver(ex: &(impl crate::db::executor::Executor + ?Sized), outbox: &mut crate::db::outbox::Outbox, sde: &crate::sde::Sde, owner: i64, ship: i64, system: i64, source: i64, pending: &crate::space::drone::PendingMining, (flag, capacity): (i32, f64)) -> Result<DeliveryOutcome, CallError> {
    let room = super::inventory::hold_storage::room(ex, sde, owner, ship, flag, capacity).await?;
    let Some(available) = crate::dao::asteroids::lock(ex, system, pending.rock, pending.type_id).await.map_err(db_err)? else { return Ok(DeliveryOutcome::Missing) };
    if available <= 0 { return Ok(DeliveryOutcome::Depleted { rock: pending.rock }); }
    let result = rounding::mining_result(pending.inputs, pending.unit_volume, available, pending.draws)
        .map_err(|error| CallError::Internal(anyhow::anyhow!("{error}")))?;
    let units = result.normal_quantity.saturating_add(result.critical_hit_quantity);
    if units <= 0 { return Ok(DeliveryOutcome::NoYield); }
    let t = sde.types.get(pending.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {}", pending.type_id)))?;
    let room = room.units(sde, flag, pending.type_id, false, units)?;
    let (taken, wasted) = extraction_amounts(units, result.wasted_quantity, available, room);
    if taken <= 0 { return Ok(DeliveryOutcome::Full { flag }); }
    let removed = taken + wasted;
    if !crate::dao::asteroids::take(ex, pending.rock, available, removed).await.map_err(db_err)? {
        return Err(CallError::Internal(anyhow::anyhow!("asteroid extraction guard failed")));
    }
    let stack = crate::dao::items::transfers::deposit_stack(
        ex, ship, flag, pending.type_id, owner, taken,
        crate::dao::items::transfers::DepositCandidate::InStatement, None,
    ).await.map_err(db_err)?;
    let receipt = super::items::enrich_deposit(sde, stack, taken)?;
    let gas = t.name.as_deref().is_some_and(|name| {
        field_groups(Field::Gas, 1.0, false).0.contains(&name)
            || field_groups(Field::Gas, 0.0, false).0.contains(&name)
            || field_groups(Field::Gas, 0.0, true).0.contains(&name)
    });
    let fact = crate::dao::mining_ledger::DeliveryFact {
        character_id: owner, ship_id: ship, solar_system_id: system, type_id: pending.type_id,
        quantity: taken, quantity_wasted: wasted, event_time: filetime_now(),
        unit_volume_m3: t.volume.unwrap_or(1.0).max(0.01),
        resource_kind: if gas { crate::lua::events::resources::MiningResourceKind::Gas } else { crate::lua::events::resources::MiningResourceKind::Ore },
    };
    crate::dao::mining_ledger::append(ex, &fact).await.map_err(db_err)?;
    for row in receipt.rows() {
        let change = match row.old_quantity {
            Some(old) => (IX_STACK_SIZE, Value::Int(old)),
            None => (IX_LOCATION_ID, Value::Int(0)),
        };
        outbox.notify_pilot(owner, "OnItemChange", vec![row.item.row(), super::inventory::notifications::item_change_dict([change]), Value::None]);
    }
    outbox.notify_pilot(owner, "OnOreMined", vec![Value::dict([
        ("oreType", Value::Int(pending.type_id as i64)), ("quantity_added", Value::Int(taken)),
        ("quantity_removed", Value::Int(taken)), ("hasRewards", Value::Bool(false)),
        ("amountWasted", Value::Int(wasted)), ("amountCritBonus", Value::Int(result.critical_hit_quantity.max(0).min(taken))),
        ("itemID", Value::Int(pending.rock)), ("moduleItemID", Value::Int(source)),
    ])]);
    Ok(DeliveryOutcome::Delivered(Delivery { rock: pending.rock, left: available - removed, removed, full: taken < units.min(available) }))
}

/// Apply only committed results to the space cache and observers.
pub(crate) async fn delivered(db: &Db, world: &Arc<World>, system: i64, owner: i64, delivery: Delivery) -> Result<(), CallError> {
    let rock = delivery.rock;
    let left = delivery.left;
    let pilots = world.with(system, move |s| {
        s.apply_mining_delivery(rock, left);
        s.pilots().filter_map(|b| b.char_id.filter(|c| *c != owner)).collect::<Vec<_>>()
    }).await.unwrap_or_default();
    for pilot in pilots {
        world.notify_pilot(pilot, "OnOreMinedByOther", vec![Value::Int(delivery.rock), Value::Int(delivery.removed)]);
    }
    if delivery.left == 0 { reconcile_depleted(db, world, system, delivery.rock).await?; }
    Ok(())
}

fn tell(notify: &NotifyTx, text: &str) {
    let _ = notify.send(("OnRemoteMessage".into(), vec![Value::str(messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(text))])]));
}

fn full_notice(flag: i32) -> &'static str {
    if flag == FLAG_CARGO { messages::CARGO_HOLD_FULL } else { messages::MINING_HOLD_FULL }
}

pub(crate) struct Extraction {
    pub repeat: bool,
    pub outcome: Option<crate::lua::events::ModuleCycleOutcome>,
    pub error: Option<CallError>,
}

enum ExtractionDelivery {
    InvalidSource,
    Delivery(DeliveryOutcome),
}

/// The end of a mining cycle of `module` on rock `target`: move the yield to the hold.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn extract(db: &Db, world: &Arc<World>, notify: &NotifyTx, char_id: i64, ship: i64, system: i64, fit: &Fit, module: i64, module_type: i32, module_flag: i32, target: i64) -> Result<Extraction, CallError> {
    use crate::lua::events::ModuleCycleOutcome;
    let facts = world.with(system, move |s| {
        let me = s.ball(ship)?;
        let position = s.sample_position(ship)?;
        Some((me.char_id == Some(char_id), s.rock_type(target), match s.ball(target) {
            None => Some(None),
            Some(it) => s.sample_position(target).map(|target_position| Some(
                dist(position, target_position) - me.radius as f64 - it.radius as f64
            )),
        }))
    }).await.flatten();
    let Some((source_valid, type_id, Some(gap))) = facts else { return Ok(Extraction { repeat: false, outcome: None, error: None }) };
    let target_lost = source_valid.then_some(ModuleCycleOutcome::TargetLost);
    let Some(gap) = gap else { return Ok(Extraction { repeat: false, outcome: target_lost, error: None }) };
    let Some(type_id) = type_id else { return Ok(Extraction { repeat: false, outcome: target_lost, error: None }) };
    if gap > fit.value(Key::Module(module), ATTR_MAX_RANGE) {
        tell(notify, messages::TARGET_OUTSIDE_MINING_RANGE);
        return Ok(Extraction { repeat: false, outcome: None, error: None });
    }
    let volume = world.sde().types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing type {type_id}")))?.volume.unwrap_or(1.0).max(0.01);
    let values = yield_values(fit, Key::Module(module));
    let destination = destination(fit, type_id);
    let db = db.clone();
    let world = world.clone();
    let notify = notify.clone();
    tokio::spawn(async move {
        let result: Result<Extraction, CallError> = async {
            let _lock = super::market::lock().await;
            let pending = yield_intent(values, target, type_id, volume);
            let sde = world.sde().clone();
            let result = db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let pending = pending.clone();
                    let sde = sde.clone();
                    Box::pin(async move {
                        if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Ok(ExtractionDelivery::InvalidSource); }
                        let hull = super::items::load_items(tx, &sde, crate::dao::items::loading::ItemQuery::Id(ship)).await.map_err(db_err)?.into_iter().next();
                        if !hull.is_some_and(|h| h.owner_id == char_id && h.location_id == system && h.singleton && h.category_id == i64::from(infinity_core::consts::CATEGORY_SHIP))
                            || !super::inventory::is_fitting_flag(module_flag)
                            || !crate::dao::items::mining::lock_module(tx, char_id, ship, module, module_type, module_flag).await.map_err(db_err)? {
                            return Ok(ExtractionDelivery::InvalidSource);
                        }
                        deliver(tx, outbox, &sde, char_id, ship, system, module, &pending, destination).await.map(ExtractionDelivery::Delivery)
                    })
                },
                |value, outbox| {
                    let world = world.clone();
                    async move {
                        crate::db::outbox::dispatch_world(value, outbox, &world)
                    }
                },
            ).await?;
            let (repeat, outcome, error) = match result {
                ExtractionDelivery::InvalidSource => (false, None, None),
                ExtractionDelivery::Delivery(DeliveryOutcome::Missing) => (false, target_lost, None),
                ExtractionDelivery::Delivery(DeliveryOutcome::Depleted { rock }) => {
                    let error = reconcile_depleted(&db, &world, system, rock).await.err();
                    (false, target_lost, error)
                }
                ExtractionDelivery::Delivery(DeliveryOutcome::NoYield) => (true, Some(ModuleCycleOutcome::Completed), None),
                ExtractionDelivery::Delivery(DeliveryOutcome::Full { flag }) => {
                    tell(&notify, full_notice(flag));
                    (false, Some(ModuleCycleOutcome::CargoFull), None)
                }
                ExtractionDelivery::Delivery(DeliveryOutcome::Delivered(result)) => {
                    let repeat = result.repeat();
                    if result.full { tell(&notify, full_notice(destination.0)); }
                    let error = delivered(&db, &world, system, char_id, result).await.err();
                    (repeat && error.is_none(), Some(ModuleCycleOutcome::Completed), error)
                }
            };
            if let Some(error) = &error { warn!(module, ?error, "mining extraction failed"); }
            Ok(Extraction { repeat, outcome, error })
        }.await;
        if let Err(error) = &result { warn!(module, ?error, "mining extraction failed"); }
        result
    }).await.map_err(|error| CallError::Internal(error.into()))?
}



pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "inSpaceCompressionMgr" => in_space_compression_mgr::dispatch(c, call).await,
        "characterMiningLedger" => ledger::dispatch(c, call).await,
        "miningScanMgr" => mining_scan_mgr::dispatch(c, call).await,
        "reprocessingSvc" => reprocessing_svc::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
