//! Transactional skill timeline integration for station clone body changes.
//! Uses game.skills, game.skill_queue, game.items and game.jobs through the shared
//! completion and training mechanisms. Body death is handled separately.
use super::{attributes::character_attributes_at, completion::{self, AdvanceResult}, trading::available_credits, training, CallError};
use crate::{db::{executor::Executor, outbox::Outbox}, sde::Sde, services::effects::online::OnlineChanged};
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
    crate::services::fit::Fit::load_character_with_skills_at(ex, Arc::clone(sde), owner, now, &plan.skills).await
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
pub(in crate::services) async fn finish_tx<E: From<OnlineChanged> + Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &Arc<Sde>, owner: i64, now: i64, mut change: BodyChange, implants_changed: bool, boosters_changed: bool) -> Result<(), CallError> {
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
    completion::finish_tx(ex, outbox, sde, owner, now, &change.progress, &[]).await
}

#[cfg(test)] mod tests;
