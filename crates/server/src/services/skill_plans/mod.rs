//! Personal and corporation skill plans and viewer-specific milestones.
//! Mirrors the public skill-plan gateway and client validation using game skill-plan tables.
//! Delete, field and requirement writes share one post-validation transaction: personal rows
//! lock the character, corporation rows authorize the current member and publish one notice.
//! Plan creation keeps its own quotas and category checks, and active-plan selection stays local.
//! Completion notifications are not provided here.
pub(crate) mod personal;
pub(crate) mod milestones;
pub(crate) mod corporation;
pub(crate) mod publication;

use std::sync::Arc;
use crate::{dao::skills::plans::{self, Owner}, db::{outbox::{self, Outbox}, Db}, space::World};

use infinity_core::consts::UUID_LEN;
fn valid_id(id: &[u8]) -> bool { id.len() == UUID_LEN && id.iter().any(|byte| *byte != 0) }

#[derive(Debug)]
pub(crate) enum Error {
    Invalid,
    Missing,
    Precondition,
    Internal(crate::services::CallError),
}

impl From<crate::db::Error> for Error {
    fn from(error: crate::db::Error) -> Self { Self::Internal(crate::services::db_err(error)) }
}
impl From<crate::services::CallError> for Error {
    fn from(error: crate::services::CallError) -> Self {
        match error {
            crate::services::CallError::User { .. } => Self::Precondition,
            error => Self::Internal(error),
        }
    }
}
impl crate::db::transaction::TransactionError for Error {
    fn is_retryable_transaction(&self) -> bool {
        match self {
            Self::Internal(error) => crate::db::transaction::TransactionError::is_retryable_transaction(error),
            _ => false,
        }
    }
}

/// The three guarded plan writes that share lock, owner and publication order.
/// Field and requirement payloads stay in the `Arc` the caller already built.
enum PlanMutation {
    Delete,
    Field { field: plans::Field, value: Arc<str> },
    Requirements(Arc<(Vec<i32>, Vec<i32>)>),
}

/// Apply one validated plan write. `None` is a personal plan; `Some` is the caller's
/// current corporation, its publication gate and the notice bound after commit.
async fn change_plan(db: &Db, character: i64, id: [u8; UUID_LEN], mutation: PlanMutation, publication: Option<(i64, &World, Arc<publication::Notice>)>) -> Result<(), Error> {
    fn prepare(mutation: &PlanMutation) -> PlanMutation { match mutation { PlanMutation::Delete => PlanMutation::Delete, PlanMutation::Field { field, value } => PlanMutation::Field { field: *field, value: Arc::clone(value) }, PlanMutation::Requirements(requirements) => PlanMutation::Requirements(Arc::clone(requirements)) } }
    async fn apply(tx: &crate::db::Transaction<'_>, id: &[u8], owner: Owner, mutation: &PlanMutation) -> Result<u64, Error> {
        plans::lock(tx, id).await?;
        Ok(match mutation {
            PlanMutation::Delete => plans::delete(tx, owner, id).await?,
            PlanMutation::Field { field, value } => plans::set_field(tx, owner, id, *field, value).await?,
            PlanMutation::Requirements(requirements) => plans::set_requirements(tx, owner, id, &requirements.0, &requirements.1).await?,
        })
    }
    match publication {
        None => db.transaction(|tx, _: &mut Outbox| {
            let mutation = prepare(&mutation);
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, character).await?;
                if apply(tx, &id, Owner::Character(character), &mutation).await? != 1 { return Err(Error::Missing); }
                Ok(())
            })
        }, |value, _| async move { Ok(value) }).await,
        Some((corporation, world, notice)) => {
            let _gate = world.notice_hub().corporation_publication(corporation).await;
            db.transaction(|tx, outbox: &mut Outbox| {
                let notice = Arc::clone(&notice);
                let mutation = prepare(&mutation);
                Box::pin(async move {
                    corporation::authorize_edit(tx, character, corporation).await?;
                    if apply(tx, &id, Owner::Corporation(corporation), &mutation).await? != 1 { return Err(Error::Missing); }
                    publication::publish(tx, outbox, corporation, notice.as_ref()).await
                })
            }, |value, outbox| async move { outbox::dispatch_world(value, outbox, world).map_err(Error::from) }).await
        }
    }
}
pub(crate) mod progress;
pub(crate) mod recovery;
pub(crate) mod validation;
#[cfg(all(test, feature = "tests-skills"))] mod tests;
