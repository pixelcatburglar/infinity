//! Authorized account-level grade grants and their post-commit notifications.
//! Writes `game.account_clone_grades`, `game.account_clone_grade_audit` and
//! `game.jobs` atomically, then notifies the active account sender after commit.
//! Client grade notifications follow `omega/client/clone_grade_svc.py`.
//! These administrative grants do not process payments or subscription commerce.
use crate::{client::filetime_now, consts::GM_ROLES, dao::accounts::subscriptions as dao, db::{outbox::{dispatch_world, Outbox}, Db}, live_sessions, messages, scheduler, services::{notify_error, CallError}, space::World};
use marshal::Value;
use super::{deadlines, rules};

pub(super) fn notify(outbox: &mut Outbox, user: i64, grade: i16, time: i64, grade_changed: bool, time_changed: bool) {
    if let Some(sender) = live_sessions::sender(user) {
        if grade_changed { outbox.notify_sender(sender.clone(), "OnSubscriptionChangedServer", vec![Value::Int(i64::from(grade))]); }
        if grade_changed || time_changed { outbox.notify_sender(sender, "OnSubscriptionTimeUpdateServer", vec![Value::Int(time)]); }
    }
}

pub(crate) async fn set(db: &Db, world: &World, actor_user: i64, actor_roles: i64, target_user: i64, grade: i16, duration: Option<i64>, grace_secs: i64) -> Result<(), CallError> {
    // This is an account operation; character selection is neither required nor authority.
    if actor_roles & GM_ROLES == 0 { return Err(notify_error(messages::subscriptions::GRADE_CHANGE_FORBIDDEN)); }
    let now = filetime_now();
    db.transaction(
        |tx, outbox: &mut Outbox| Box::pin(async move {
            let before = dao::lock_opt(tx, target_user).await?.ok_or_else(|| notify_error(messages::subscriptions::ACCOUNT_NOT_FOUND))?;
            let (omega_until, grace_until) = rules::grant_deadlines(grade, duration, grace_secs, now)?;
            let old_grade = rules::effective_grade(&before, now);
            let old_time = rules::subscription_time(&before, now)?;
            if !dao::set_grade(tx, target_user, grade, omega_until, grace_until, before.revision).await? {
                return Err(CallError::Internal(anyhow::anyhow!("clone-grade revision conflict or overflow")));
            }
            dao::audit(tx, target_user, Some(actor_user), &before, grade, omega_until, grace_until, "grant").await?;
            let after = dao::CloneGrade { user_id: target_user, grade, omega_until, grace_until, revision: before.revision + 1 };
            deadlines::schedule(tx, &after).await?;
            let new_grade = rules::effective_grade(&after, now);
            let new_time = rules::subscription_time(&after, now)?;
            let stale_expired = before.grade == rules::OMEGA && old_grade == rules::ALPHA;
            notify(outbox, target_user, new_grade, new_time, old_grade != new_grade || stale_expired, old_time != new_time);
            Ok(())
        }),
        |value, outbox| async move { let result = dispatch_world(value, outbox, world); scheduler::wake(); result },
    ).await
}
