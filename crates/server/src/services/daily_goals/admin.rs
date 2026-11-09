//! Authorized, idempotent QA progress for the client goal messenger.
//! Uses account roles and persisted daily-goal audit receipts; no session role
//! or client-supplied authority grants access.
use std::sync::Arc;
use infinity_core::{consts::{ROLE_QA, UUID_LEN}};
use crate::{config::game::daily_goals::DailyGoalsConfig, dao::{accounts::authentication, characters::persistence, daily_goals::receipts::{self, AuditRow}}, db::{executor::Executor, outbox::{self, Outbox}, transaction::TransactionError}, progress::ApplyError, services::identity::Identity};
use super::{progress::admin as planner, Error, Resources, DAILY_GOALS_NAMESPACE};

async fn authority(ex: &(impl Executor + ?Sized), user: i64, character: i64, config: &DailyGoalsConfig) -> Result<(), Error> {
    if user <= 0 || character <= 0 || !config.qa_account_ids.contains(&user)
        || !persistence::owned(ex, user, character).await?
        || !authentication::role(ex, user).await?.is_some_and(|role| role & ROLE_QA != 0) {
        return Err(Error::Forbidden);
    }
    Ok(())
}

/// Validate the original request before invoking Lua.
pub(crate) async fn authorize(resources: &Resources, identity: &Identity, user: i64, request_uuid: &[u8], config: &DailyGoalsConfig) -> Result<(), Error> {
    if request_uuid.len() != UUID_LEN { return Err(Error::Invalid); }
    let session = resources.world.online_session(identity.char_id).ok_or(Error::Forbidden)?;
    if session.user_id != Some(user) || session.character_id != identity.char_id { return Err(Error::Forbidden); }
    authority(&resources.db, user, identity.char_id, config).await
}

#[derive(Debug)]
enum TxError { Stale, Domain(Error) }
impl From<crate::db::Error> for TxError { fn from(e: crate::db::Error) -> Self { Self::Domain(e.into()) } }
impl From<Error> for TxError { fn from(e: Error) -> Self { Self::Domain(e) } }
impl From<ApplyError> for TxError {
    fn from(e: ApplyError) -> Self { match e { ApplyError::Reprepare => Self::Stale, ApplyError::Call(e) => Self::Domain(Error::Internal(e)) } }
}
impl TransactionError for TxError {
    fn is_retryable_transaction(&self) -> bool { match self { Self::Domain(e) => e.is_retryable_transaction(), Self::Stale => false } }
}
fn matches(row: &AuditRow, actor: i64, target: i64, goal: &str, payload: &serde_json::Value) -> Result<bool, Error> {
    let stored: serde_json::Value = serde_json::from_str(&row.payload).map_err(anyhow::Error::from)?;
    Ok(row.actor_id == actor && row.subject_id == target && row.goal_id == goal && row.operation == "progress" && &stored == payload)
}

/// Apply one UUID-scoped increment and its audit in the same transaction.
pub(crate) async fn progress(resources: &Resources, identity: &Identity, user: i64, request_uuid: &[u8], target: i64, goal: &str, increment: u64, config: &DailyGoalsConfig, at: i64) -> Result<(), Error> {
    authorize(resources, identity, user, request_uuid, config).await?;
    let actor = identity.char_id;
    let config = Arc::new(config.clone());
    if target != identity.char_id { return Err(Error::Forbidden); }
    let goal = crate::uuid_text::format(&crate::uuid_text::parse(goal, crate::uuid_text::Case::Either).ok_or(Error::Invalid)?);
    let uuid = crate::uuid_text::format(request_uuid.try_into().map_err(|_| Error::Invalid)?);
    let payload = serde_json::json!({"increment": increment.to_string()});
    let current = crate::lua::content_policy(DAILY_GOALS_NAMESPACE)?;
    let _admission = crate::lua::admit_content(&current)?;
    loop {
        authorize(resources, identity, user, request_uuid, &config).await?;
        if let Some(existing) = receipts::audit(&resources.db, &uuid).await? {
            return if matches(&existing, identity.char_id, target, &goal, &payload)? { Ok(()) } else { Err(Error::Invalid) };
        }
        let prepared = planner::prepare(resources, user, target, &goal, increment, at).await?;
        let audit = AuditRow { audit_id: uuid.clone(), actor_id: identity.char_id, subject_id: target, goal_id: goal.clone(), operation: "progress".into(), payload: payload.to_string(), before_state: prepared.before, after_state: prepared.after, recorded_at: at };
        let prepared = Arc::new(prepared.change);
        let audit = Arc::new(audit);
        let payload = Arc::new(payload.clone());
        let goal = Arc::new(goal.clone());
        let result = resources.db.transaction::<_, _, TxError, Outbox, _, _, _>(|tx, outbox| {
            let prepared = Arc::clone(&prepared);
            let audit = Arc::clone(&audit);
            let payload = Arc::clone(&payload);
            let goal = Arc::clone(&goal);
            let config = Arc::clone(&config);
            Box::pin(async move {
                prepared.prelock(tx).await?;
                authority(tx, user, target, &config).await?;
                if let Some(existing) = receipts::audit(tx, &audit.audit_id).await? {
                    return if matches(&existing, actor, target, &goal, &payload)? { Ok(()) } else { Err(TxError::Domain(Error::Invalid)) };
                }
                if !receipts::insert_audit(tx, &audit).await? {
                    let existing = receipts::audit(tx, &audit.audit_id).await?;
                    return match existing { Some(row) if matches(&row, actor, target, &goal, &payload)? => Ok(()), Some(_) => Err(TxError::Domain(Error::Invalid)), None => Err(TxError::Domain(Error::Unavailable)) };
                }
                prepared.apply(tx, outbox).await?;
                Ok(())
            })
        }, |value, outbox| async move { outbox::dispatch_world(value, outbox, &resources.world).map_err(|e| TxError::Domain(Error::Internal(e))) }).await;
        match result { Ok(()) => return Ok(()), Err(TxError::Stale) => continue, Err(TxError::Domain(error)) => return Err(error) }
    }
}

#[cfg(test)] mod tests;
