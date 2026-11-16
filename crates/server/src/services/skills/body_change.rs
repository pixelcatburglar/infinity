//! Transactional skill timeline integration for station clone body changes.
//! Uses game.skills, game.skill_queue, game.items and game.jobs through the shared
//! completion and training mechanisms. A piloted capsule death additionally
//! removes the owner's unprotected active augmentation rows in the same
//! transaction and finishes the body through the medical rebirth; dormant clone
//! custody is never touched here.
//! Also hosts the shared augmentation attribute lookup used by clone custody transitions.
use super::{attributes::character_attributes_at, completion::{self, AdvanceResult}, trading::available_credits, training, CallError};
use infinity_core::consts::{dogma::ATTR_NON_DESTRUCTIBLE, FLAG_IMPLANT};
use crate::{dao::items::brain::{self, BrainItem}, db::{executor::Executor, outbox::Outbox}, sde::Sde, services::{db_err, effects::online::OnlineChanged, items::load_items}};
use marshal::Value;
use std::{collections::HashMap, sync::Arc};

/// Timeline progress and effective attributes captured before changing bodies.
pub(in crate::services) struct BodyChange {
    progress: AdvanceResult,
    before: HashMap<i64, f64>,
    credits_before: i64,
}
/// Project effective character dogma without mutating the skill timeline.
/// The caller holds the character lock and supplies a consistent transaction.
pub(in crate::services) async fn projected_fit_tx(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, owner: i64, now: i64) -> Result<crate::services::fit::Fit, CallError> {
    let plan = super::timeline::plan_tx(ex, sde, owner, now).await?;
    crate::services::fit::Fit::load_character_with_skills_at(ex, Arc::clone(sde), owner, now, &plan.skills, &plan.initial_installations).await
}


/// Caller holds the market guard and character lock for the transaction.
pub(in crate::services) async fn begin_tx<E>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, owner: i64, now: i64) -> Result<BodyChange, CallError> {
    let progress = completion::advance_tx(ex, outbox, sde, owner, now).await?;
    let before = character_attributes_at(ex, sde, owner, now).await?;
    let credits_before = available_credits(ex, sde, owner, now).await?;
    Ok(BodyChange { progress, before, credits_before })
}

/// Finish after the body mutations, with the same locks and timestamp as `begin_tx`.
/// Notifications are queued in the transaction and dispatched by the caller.
pub(in crate::services) async fn finish_tx<E: From<OnlineChanged> + Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, fences: &crate::services::effects::online::OnlineFences, owner: i64, now: i64, mut change: BodyChange, implants_changed: bool, boosters_changed: bool) -> Result<(), CallError> {
    training::retime_after_change_tx(ex, sde, owner, now, &change.before, &mut change.progress).await?;
    change.progress.brain_changed |= implants_changed || boosters_changed;
    change.progress.boosters_changed |= boosters_changed;
    if boosters_changed {
        let after = available_credits(ex, sde, owner, now).await?;
        if change.credits_before > after {
            outbox.notify_pilot(owner, "OnNonDiminishingInjectionsRemoved", vec![]);
            if after > 0 { outbox.notify_pilot(owner, "OnNonDiminishingInjectionsAdded", vec![Value::Int(after)]); }
        }
    }
    if implants_changed { outbox.notify_pilot(owner, "OnServerImplantsChanged", vec![]); }
    completion::finish_tx(ex, outbox, sde, fences, owner, now, &change.progress, &[]).await
}

/// Effective dogma attribute for one augmentation item: instance override, then type value.
pub(in crate::services) fn attribute(sde: &Sde, item: &BrainItem, overrides: &HashMap<(i64, i32), f64>, attr: i32) -> Option<f64> {
    overrides.get(&(item.item_id, attr)).copied().or_else(|| sde.types.attribute(item.type_id, attr))
}

/// A piloted body death after its timeline settlement, carried to the medical
/// rebirth so the finish can run once the replacement capsule exists.
pub(in crate::services) struct BodyLoss {
    change: BodyChange,
    implants_changed: bool,
    boosters_changed: bool,
}

/// Refresh categories a verified-commit reconcile replays after the session change.
#[derive(Clone, Copy)]
pub(in crate::services) struct BodyRefresh {
    pub implants_changed: bool,
    pub boosters_changed: bool,
}

/// Settle the skill timeline at `now`, then delete every active brain item
/// whose non-destruction protection resolves to zero: instance override, then
/// type attribute, then the SDE attribute default, then zero. A nonzero finite
/// value protects its row, so an instance override of zero overrides type
/// protection. Naturally expired rows were already consumed by the timeline
/// settlement and never count as death losses. Each row the guarded delete
/// actually removed reaches the caller's `destroyed` callback and an
/// owner-addressed removal notification; dormant clone custody is untouched.
pub(in crate::services) async fn begin_loss_tx<E: Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, owner: i64, now: i64,
    mut destroyed: impl FnMut(&BrainItem) + Send,
) -> Result<BodyLoss, CallError> {
    let change = begin_tx(ex, outbox, sde, owner, now).await?;
    let active = brain::list(ex, owner).await.map_err(db_err)?;
    let overrides: HashMap<(i64, i32), f64> = crate::dao::items::dogma::overrides_for(ex,
        &active.iter().map(|item| item.item_id).collect::<Vec<_>>()).await.map_err(db_err)?
        .into_iter().map(|a| ((a.item_id, a.attribute_id), a.value)).collect();
    let mut doomed = Vec::new();
    let mut pending = Vec::new();
    for item in &active {
        let protection = attribute(sde, item, &overrides, ATTR_NON_DESTRUCTIBLE)
            .or_else(|| sde.dogma.attribute(ATTR_NON_DESTRUCTIBLE).and_then(|a| a.default_value))
            .unwrap_or(0.0);
        if !protection.is_finite() {
            return Err(CallError::Internal(anyhow::anyhow!("non-finite augmentation destruction protection")));
        }
        if protection == 0.0 { doomed.push(item.item_id); pending.push(item); }
    }
    let mut descriptors = if doomed.is_empty() { Vec::new() } else {
        load_items(ex, sde, crate::dao::items::loading::ItemQuery::Ids(&doomed)).await.map_err(db_err)?
    };
    descriptors.sort_unstable_by_key(|item| item.item_id);
    if descriptors.len() != pending.len() {
        return Err(CallError::Internal(anyhow::anyhow!("destroyed augmentation descriptor set changed")));
    }
    let (mut implants_changed, mut boosters_changed) = (false, false);
    for (item, descriptor) in pending.into_iter().zip(descriptors) {
        if descriptor.item_id != item.item_id {
            return Err(CallError::Internal(anyhow::anyhow!("destroyed augmentation descriptor mismatch")));
        }
        if !brain::remove(ex, owner, item).await.map_err(db_err)? {
            return Err(db_err(crate::db::Error::StaleSnapshot("active brain changed during loss")));
        }
        destroyed(item);
        if item.flag_id == FLAG_IMPLANT { implants_changed = true; } else { boosters_changed = true; }
        crate::services::inventory::notifications::notify_owner_item_removed(outbox, descriptor, owner);
    }
    Ok(BodyLoss { change, implants_changed, boosters_changed })
}

/// Finish a body death after the replacement capsule is in place: the shared
/// change finish, then one clone-destruction notice. The returned categories
/// include booster changes the timeline settlement itself processed.
pub(in crate::services) async fn finish_loss_tx<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>,
    fences: &crate::services::effects::online::OnlineFences, owner: i64, now: i64,
    loss: BodyLoss,
) -> Result<BodyRefresh, CallError> {
    let boosters_changed = loss.boosters_changed || loss.change.progress.boosters_changed;
    let refresh = BodyRefresh { implants_changed: loss.implants_changed, boosters_changed };
    finish_tx(ex, outbox, sde, fences, owner, now, loss.change, loss.implants_changed, boosters_changed).await?;
    outbox.notify_pilot(owner, "OnCloneDestruction", vec![]);
    Ok(refresh)
}

#[cfg(all(test, feature = "tests-skills"))] mod tests;
