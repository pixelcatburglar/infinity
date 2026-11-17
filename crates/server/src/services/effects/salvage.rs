//! Salvager cycles and transactional wreck recovery.
//! The client dogma salvaging effect uses fitted accessDifficultyBonus and the
//! wreck's SDE accessDifficulty. Native wreck yield tables do not ship: rewards
//! are a configured random selection of published Salvaged Materials types.
//! Cargo occupancy follows `inventorycommon.util.GetItemVolume` for each item.
//! Loose wreck contents are retained in a positioned cargo container.
//! Successful wreck recovery journals a WreckSalvaged fact atomically with inventory changes,
//! using the wreck ball and transactional character/ship data. The durable outcome
//! is authoritative; live container publication is best effort and failures are
//! warned, with no automatic recovery.
//! Effect animations are published through the system's ordered scene channel.
use super::{lifecycle::{Cycle, CycleOutcome, Module}, parameters::MasterPacket, registry::{Effect, EffectAttribute, EffectFlag}};
use infinity_core::consts::{dogma::ATTR_MAX_RANGE, FLAG_CARGO, GROUP_CARGO_CONTAINER, GROUP_WRECK, IX_LOCATION_ID};
use crate::dao::items::{creation, inventory, loading::{self, ItemQuery}, salvage};
use crate::db::outbox::{Effect as OutboxEffect, Outbox};
use infinity_core::messages;
use crate::services::{combat::{Ctx, state, targeting_charges::targets_of}, db_err, fit::{Fit, Key, PERCENT_SCALE}, inventory::notifications::notify_item_change, items::load_items, market, rand01, user_error, CallError};
use crate::space::{dist, Ball, FxModule, System, MODE_RIGID, MODE_WARP, system::presentation::{FxRetention, ModuleFx}};
use marshal::Value;
use std::sync::Arc;
use crate::lua::events::ModuleCycleOutcome;
use crate::db::outbox::SourceKey;
use crate::lua::events::{GameEvent, resources::{FactContext, FactOrigin}};

/// SDE dgmAttributeTypes: accessDifficulty, highIsGood=true, unit 121 (realPercent).
/// The additive sign and percent scaling are server policy.
const ATTR_ACCESS_DIFFICULTY: i32 = 901;
/// SDE invGroups: Salvaged Materials, excluding Ancient Salvage.
const GROUP_SALVAGED_MATERIALS: i32 = 754;

fn chance(bonus: f64, difficulty: f64) -> f64 {
    ((bonus + difficulty) / PERCENT_SCALE).clamp(0.0, 1.0)
}

fn active(master: i64, generation: &Arc<std::sync::atomic::AtomicBool>, character: i64, target: i64) -> bool {
    let current = state().active.get(&master).is_some_and(|active| Arc::ptr_eq(&active.stop, generation));
    current && targets_of(character).contains(&target)
}

fn range(fit: &Fit, module: &Module, effect: &Effect) -> f64 {
    fit.value(Key::Module(module.id), effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE))
}

fn target_ball(s: &System, source: i64, target: i64, range: f64) -> Option<Ball> {
    if source == target || !range.is_finite() || range < 0.0 { return None; }
    let (a, b) = s.sampled_ball(source).zip(s.sampled_ball(target))?;
    if a.mode == MODE_WARP || b.mode == MODE_WARP || (dist(a.pos, b.pos) - f64::from(a.radius) - f64::from(b.radius)).max(0.0) > range { return None; }
    Some(b)
}

pub(super) async fn validate(cx: &Ctx, fit: &Fit, module: &Module, effect: &Effect, target: Option<i64>) -> Result<(), CallError> {
    check(cx, range(fit, module, effect), target).await
}

async fn check(cx: &Ctx, range: f64, target: Option<i64>) -> Result<(), CallError> {
    let refuse = || user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]);
    let target = target.filter(|id| targets_of(cx.char_id).contains(id)).ok_or_else(refuse)?;
    let item = load_items(&cx.db, cx.world.sde(), ItemQuery::Id(target)).await.map_err(db_err)?.into_iter().next().ok_or_else(refuse)?;
    if item.group_id != i64::from(GROUP_WRECK) || item.location_id != cx.system_id { return Err(refuse()); }
    let source = cx.ship_id;
    if cx.world.with(cx.system_id, move |s| target_ball(s, source, target, range)).await.flatten().is_none() { return Err(refuse()); }
    Ok(())
}

pub(super) async fn start(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    if !super::lifecycle::cycle_subject_valid(cycle).await { return Ok(CycleOutcome::Stop("target lost")); }
    let Some(range) = cycle.inputs.frame.as_ref().and_then(|frame| match &frame.packet {
        MasterPacket::Salvage(inputs) => Some(inputs.range),
        _ => None,
    }) else {
        return Err(CallError::Internal(anyhow::anyhow!("salvage cycle captured without salvage inputs")));
    };
    if check(cx, range, cycle.target).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    if cycle.cycles == 0 {
        if let Some(guid) = cycle.effect.guid.as_deref().and_then(|g| g.split(',').next()).map(str::to_owned) {
            let (modules, master, generation, duration, repeat, offensive) = (cycle.modules.clone(), cycle.modules[0].id, cycle.stop.clone(), cycle.duration_ms, cycle.repeat, cycle.effect.has(EffectFlag::Offensive));
            cx.world.post(cx.system_id, move |s| {
                for m in &modules {
                    s.publish_module_fx(master, &generation, ModuleFx {
                        module: FxModule { item: Some(m.id), type_id: m.type_id },
                        guid: guid.clone(), charge: None, offensive,
                        duration_ms: duration, repeat, graphic_info: Value::None,
                    }, true, FxRetention::Current);
                }
            });
        }
    }
    Ok(CycleOutcome::Continue)
}

pub(super) async fn end(cx: &Ctx, cycle: &mut Cycle) -> Result<CycleOutcome, CallError> {
    let Some(target) = cycle.target else { return Ok(CycleOutcome::Stop("target lost")); };
    if !super::lifecycle::cycle_subject_valid(cycle).await { return Ok(CycleOutcome::Stop("target lost")); }
    let Some((range, bonus)) = cycle.inputs.frame.as_ref().and_then(|frame| match &frame.packet {
        MasterPacket::Salvage(inputs) => Some((inputs.range, inputs.bonus)),
        _ => None,
    }) else {
        return Err(CallError::Internal(anyhow::anyhow!("salvage cycle captured without salvage inputs")));
    };
    if check(cx, range, Some(target)).await.is_err() { return Ok(super::lifecycle::target_loss(cycle).await); }
    let (source, character, master, generation) = (cx.ship_id, cx.char_id, cycle.modules[0].id, cycle.stop.clone());
    let Some(membership) = cycle.source_membership else { return Ok(CycleOutcome::Stop("target lost")); };
    let module = cycle.modules[0].clone();
    let effect = cycle.effect.id;
    let generation_check = generation.clone();
    let ball = cx.world.with(cx.system_id, move |s| {
        let subject = super::lifecycle::source_subject_current(s, source, character, membership, &module, effect, &generation_check)
            && targets_of(character).contains(&target);
        if !subject { return None; }
        target_ball(s, source, target, range)
    }).await.flatten();
    let Some(ball) = ball else { return Ok(super::lifecycle::target_loss(cycle).await); };
    let sde = cx.world.sde().clone();
    let Some(difficulty) = sde.types.attribute(ball.type_id, ATTR_ACCESS_DIFFICULTY) else { return Ok(CycleOutcome::Stop("wreck difficulty missing")); };
    if rand01() >= chance(bonus, difficulty) { return Ok(CycleOutcome::Continue); }
    let mut pool: Vec<_> = sde.types.group_types(GROUP_SALVAGED_MATERIALS).iter().copied().filter(|id| sde.types.get(*id).is_some_and(|t| t.published == Some(true))).collect();
    if pool.is_empty() { return Err(CallError::Internal(anyhow::anyhow!("published salvage materials are missing"))); }
    let policy = &crate::config::game().salvage;
    let count = (policy.salvage_material_stacks as usize).min(pool.len());
    let mut yields = Vec::with_capacity(count);
    for i in 0..count {
        let j = i + (rand01() * (pool.len() - i) as f64) as usize;
        pool.swap(i, j);
        let width = u64::from(policy.salvage_quantity_max) - u64::from(policy.salvage_quantity_min) + 1;
        let quantity = u64::from(policy.salvage_quantity_min) + (rand01() * width as f64) as u64;
        yields.push((pool[i], quantity as i64));
    }
    let container = sde.types.iter().filter(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).min_by_key(|t| t.type_id).map(|t| (t.type_id, t.radius));

    let _lock = market::lock().await;
    let capacity = match cycle.inputs.frame.as_mut().and_then(|frame| match &mut frame.packet {
        MasterPacket::Salvage(inputs) => Some(&mut inputs.capacity),
        _ => None,
    }) {
        Some(Ok(capacity)) => *capacity,
        Some(Err(error)) => return Err(std::mem::replace(error, CallError::Internal(anyhow::anyhow!("salvage capacity already consumed")))),
        None => return Err(CallError::Internal(anyhow::anyhow!("salvage cycle captured without salvage inputs"))),
    };
    let world = cx.world.clone();
    let sender = cx.notify.clone();
    let system = cx.system_id;
    let outcome = cx.db.transaction(
        move |tx, outbox: &mut Outbox<Option<Ball>>| {
            let (sde, yields, ball, generation) = (sde.clone(), yields.clone(), ball.clone(), generation.clone());
            Box::pin(async move {
                if !active(master, &generation, character, target) { return Ok(false); }
                let additions: Vec<_> = yields.iter().map(|(type_id, quantity)| crate::services::inventory::hold_storage::Addition {
                    type_id: *type_id, singleton: false, quantity: *quantity,
                }).collect();
                let locks = match crate::services::inventory::hold_storage::check_destination(tx, &sde, character, source, FLAG_CARGO, &additions, Default::default(), crate::services::inventory::hold_storage::CapacitySource::Captured(capacity)).await {
                    Ok(Some(locks)) => locks,
                    Ok(None) => return Err(crate::services::notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION)),
                    Err(CallError::User { msg, .. }) if msg == messages::MSG_NOT_ENOUGH_CARGO_SPACE => return Ok(false),
                    Err(error) => return Err(error),
                };
                if !salvage::claim(tx, target, system, ball.type_id).await.map_err(db_err)? { return Ok(false); }
                let loot = load_items(tx, &sde, ItemQuery::Location(target)).await.map_err(db_err)?;
                if loot.len() != loading::load_items(tx, ItemQuery::Location(target)).await.map_err(db_err)?.len() {
                    return Err(CallError::Internal(anyhow::anyhow!("wreck contains an item with missing SDE type")));
                }
                let preserved = if loot.is_empty() { None } else {
                    let (kind, radius) = container.ok_or_else(|| CallError::Internal(anyhow::anyhow!("cargo container type missing")))?;
                    let id = creation::positioned(tx, kind, ball.owner_id, system, ball.pos).await.map_err(db_err)?;
                    for item in loot {
                        if !inventory::move_item(tx, &item, id, item.flag_id, item.owner_id, item.singleton).await.map_err(db_err)? { return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![])); }
                        notify_item_change(tx, &sde, outbox, item.item_id, vec![(IX_LOCATION_ID, Value::Int(target))]).await?;
                    }
                    let mut replacement = ball.clone();
                    replacement.id = id; replacement.type_id = kind; replacement.mode = MODE_RIGID;
                    replacement.radius = radius.unwrap_or(f64::from(ball.radius)) as f32;
                    replacement.vel = [0.0; 3]; replacement.old_vel = [0.0; 3]; replacement.old_pos = replacement.pos;
                    replacement.slim.clear();
                    Some(replacement)
                };
                for (kind, quantity) in yields {
                    crate::services::items::deposit_held(tx, &sde, outbox, &crate::services::inventory::destination::Destination::owned(character, source, FLAG_CARGO), kind, quantity, target, Some(&locks), crate::services::inventory::hold_storage::CapacitySource::Captured(capacity)).await?;
                }
                if !active(master, &generation, character, target) {
                    return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
                }
                let org = crate::dao::characters::members::organization(tx, character).await.map_err(db_err)?;
                let ship_type_id = loading::load_items(tx, ItemQuery::Id(source)).await.map_err(db_err)?.first().map(|item| item.type_id);
                outbox.event_from(SourceKey::new("WreckSalvaged", target), GameEvent::WreckSalvaged {
                    char_id: character, wreck_id: target, wreck_type: ball.type_id, occurred_at: crate::client::filetime_now(),
                    context: FactContext { corporation_id: org.as_ref().map(|o| o.corporation_id), alliance_id: org.and_then(|o| o.alliance_id), faction_id: None, ship_id: Some(source), ship_type_id, system_id: Some(system), station_id: None, origin: FactOrigin::Natural },
                });
                outbox.operation(preserved);
                Ok(true)
            })
        },
        move |value, outbox| async move {
            for entry in outbox.into_entries() {
                match entry {
                    OutboxEffect::Notification(notification) => notification.dispatch_sender(&world, Some(&sender))?,
                    OutboxEffect::Operation(replacement) => {
                        world.remove_ball(system, target).await;
                        if let Some(ball) = replacement {
                            let replacement_id = ball.id;
                            if let Err(error) = world.add_ship_locked(system, ball, &_lock).await {
                                tracing::warn!(system, wreck_id = target, replacement_id, %error, "failed to publish salvaged wreck container");
                            }
                        }
                    }
                }
            }
            Ok(value)
        },
    ).await?;
    Ok(if outcome { CycleOutcome::Ended { reason: "salvage complete", outcome: ModuleCycleOutcome::Completed } } else { CycleOutcome::Stop("salvage unavailable") })
}

pub(super) fn stop(s: &mut System, _cx: &Ctx, cycle: &Cycle) {
    s.retire_fx_activation(cycle.modules[0].id, &cycle.stop);
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
