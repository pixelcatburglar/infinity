//! Daily Alpha injector cooldowns in `game.character_alpha_injections` and `game.jobs`.
//! `skillsvc.py` consumes GetNextAlphaInjectionDateTime and OnAlphaInjectionAvailable;
//! other Alpha gameplay restrictions are not modelled here.
use crate::{client::filetime_now, consts::FILETIME_DAY, dao::skills::alpha_injections as dao, db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db}, messages, scheduler::{self, Job}, services::{db_err, identity::Identity, notify_error, user_error, CallError}, space::World};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};

/// Client inventorycommon/const.py daily Alpha injector.
pub(crate) const TYPE_ID: i32 = 46375;
pub(crate) const KIND: &str = "alpha-injection-available";
/// Server policy: one use per rolling 24 hours, measured from settlement.
const COOLDOWN: i64 = FILETIME_DAY;

#[derive(Clone, Serialize, Deserialize)]
struct Payload { char_id: i64, expected: i64 }

pub(crate) async fn next(identity: &Identity, db: &Db) -> Result<i64, CallError> {
    if identity.char_id <= 0 { return Err(super::trading::refusal()); }
    dao::next(db, identity.char_id).await.map_err(db_err)
}

pub(super) async fn check_tx(tx: &(impl Executor + ?Sized), char_id: i64, grade: i16, now: i64) -> Result<i64, CallError> {
    if grade != crate::services::subscriptions::rules::ALPHA {
        return Err(notify_error(messages::skills::ALPHA_INJECTOR_REQUIRES_ALPHA));
    }
    if dao::next(tx, char_id).await.map_err(db_err)? > now {
        return Err(user_error(messages::MSG_ALREADY_INJECTED_TODAY, vec![]));
    }
    now.checked_add(COOLDOWN).ok_or_else(super::trading::refusal)
}

pub(super) async fn claim_tx(tx: &(impl Executor + ?Sized), char_id: i64, now: i64, expected: i64) -> Result<(), CallError> {
    if !dao::claim(tx, char_id, now, expected).await.map_err(db_err)? {
        return Err(user_error(messages::MSG_ALREADY_INJECTED_TODAY, vec![]));
    }
    let payload = Payload { char_id, expected };
    let job = Job { id: format!("{KIND}:{char_id}:{expected}"), kind: KIND.into(), due_at: expected, payload: serde_json::to_string(&payload).map_err(db_err)? };
    scheduler::schedule(tx, &job).await.map_err(db_err)?;
    Ok(())
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Payload = serde_json::from_str(&job.payload)?;
        let now = filetime_now();
        let result = db.transaction(|tx, outbox: &mut Outbox| {
            let payload = payload.clone();
            Box::pin(async move {
                if dao::mark_available(tx, payload.char_id, payload.expected, now).await.map_err(db_err)? {
                    outbox.notify_pilot(payload.char_id, "OnAlphaInjectionAvailable", vec![]);
                }
                Ok(())
            })
        }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await;
        result.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("Alpha injector availability refused: {msg}"),
        })
    })
}
