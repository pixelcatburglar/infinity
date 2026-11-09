//! Atomic EverMark completion convenience for current ordinary daily goals.
//! The client formula is `completion_convenience/util.py`; payment receipts and
//! natural progress use the daily-goal and shared progress journals respectively.
//! This operation completes but never redeems the goal.
use std::sync::Arc;
use infinity_core::consts::CORP_HERALDRY;
use crate::{dao::{accounts::subscriptions, daily_goals::{instances, receipts::{self, PaymentRow}}, progress as journal}, db::{outbox::{self, Outbox}, transaction::TransactionError}, lua::events::resources::{FactContext, FactOrigin, LoyaltyReason}, progress::{self, ApplyError, ApplyOutcome, ClaimMode}, services::{identity::Identity, market, standings::{self, LoyaltyMutation}, subscriptions::rules}};
use super::{progress::payment::{self, BatchError}, queries, Error, Resources, DAILY_GOALS_NAMESPACE};

/// `get_remaining_completion_convenience_num` in completion_convenience/util.py.
const DAILY_COMPLETION_LIMIT: u64 = 2;
/// `PRICE_FIRST_COMPLETION` in dailygoals/client/const.py.
const FIRST_PRICE: i64 = 2500;
/// `PRICE_SECOND_COMPLETION` in dailygoals/client/const.py.
const SECOND_PRICE: i64 = 4500;
fn remaining(bonus: u64, paid: u64) -> u64 { DAILY_COMPLETION_LIMIT.saturating_sub(bonus.max(paid)) }
fn price(paid: u64) -> Option<i64> { match paid { 0 => Some(FIRST_PRICE), 1 => Some(SECOND_PRICE), _ => None } }

#[derive(Debug)]
enum TxError { Stale, Domain(Error) }
impl From<crate::db::Error> for TxError { fn from(e: crate::db::Error) -> Self { Self::Domain(e.into()) } }
impl From<Error> for TxError { fn from(e: Error) -> Self { Self::Domain(e) } }
impl From<ApplyError> for TxError { fn from(e: ApplyError) -> Self { match e { ApplyError::Reprepare => Self::Stale, ApplyError::Call(e) => Self::Domain(e.into()) } } }
impl TransactionError for TxError { fn is_retryable_transaction(&self) -> bool { match self { Self::Stale => false, Self::Domain(e) => e.is_retryable_transaction() } } }
#[derive(Clone, Copy)]
enum Outcome { Paid, Natural }

/// Replay an original successful receipt before applying present-day restrictions.
pub(crate) async fn pay_for_completion(resources: &Resources, identity: &Identity, user: i64, id: &str, at: i64) -> Result<(), Error> {
    let owner = identity.char_id;
    let id = crate::uuid_text::format(&crate::uuid_text::parse(id, crate::uuid_text::Case::Either).ok_or(Error::Invalid)?);
    loop {
        if receipts::payment(&resources.db, owner, &id).await?.is_some() { return Ok(()); }
        // New payments validate the owner and resolve the goal's retained policy.
        let _goal = queries::read_one(resources, user, owner, &id, at).await?;
        let current = crate::lua::content_policy(DAILY_GOALS_NAMESPACE)?;
        let _admission = crate::lua::admit_content(&current)?;
        let batch = match payment::prepare(resources, user, owner, &id, at).await {
            Ok(batch) => batch, Err(BatchError::Reprepare) => continue, Err(BatchError::Domain(error)) => return Err(error),
        };
        let batch = Arc::new(batch);
        let sde = Arc::clone(&resources.sde);
        let _market = market::lock().await;
        let result = resources.db.transaction::<_, _, TxError, Outbox, _, _, _>(|tx, outbox| {
            let batch = Arc::clone(&batch);
            let id = id.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                let id = &id;
                batch.prelock(tx).await?;
                let snapshot = journal::pending_for_character(tx, super::progress::registration().consumer_key, owner, (super::progress::prepare::MAX_PAYMENT_PENDING_DELIVERIES + 1) as i64).await?;
                if snapshot.as_slice() != batch.keys() { return Err(TxError::Stale); }
                let mut locked = batch.keys().to_vec();
                locked.sort_by(|a, b| (&a.consumer_key, &a.event_id).cmp(&(&b.consumer_key, &b.event_id)));
                for key in &locked { if journal::lock_pending(tx, key).await?.is_none() { return Err(TxError::Stale); } }
                let registration = super::progress::registration();
                for plan in batch.plans() {
                    if !matches!(progress::apply_once_tx(tx, outbox, &sde, &registration, plan.key().clone(), Some(plan), ClaimMode::Blocking).await?, ApplyOutcome::Applied) { return Err(TxError::Stale); }
                }
                let actual = instances::get(tx, owner, id).await?.ok_or(Error::Missing)?;
                if &actual != batch.target() { return Err(TxError::Stale); }
                let bonus = instances::get(tx, owner, batch.bonus_id()).await?.ok_or(Error::Unavailable)?;
                if bonus.progress != batch.bonus_progress() { return Err(TxError::Stale); }
                if receipts::payment(tx, owner, id).await?.is_some() { return Err(TxError::Stale); }
                if actual.completed_at.is_some() { return Ok(Outcome::Natural); }
                let now = crate::client::filetime_now();
                if now < actual.period_start || now >= actual.period_end || actual.voided_at.is_some() { return Err(Error::Forbidden.into()); }
                let grade = subscriptions::lock(tx, user).await?;
                if batch.omega_required() && !actual.earned_omega && rules::effective_grade(&grade, now) != rules::OMEGA { return Err(Error::Forbidden.into()); }
                let paid = u64::try_from(receipts::payment_count(tx, owner, actual.period_start).await?).map_err(|_| Error::Invalid)?;
                if remaining(bonus.progress, paid) == 0 { return Err(Error::Forbidden.into()); }
                let amount = price(paid).ok_or(Error::Forbidden)?;
                let mutation = LoyaltyMutation { reason: LoyaltyReason::DailyCompletion, occurred_at: at, context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural } };
                standings::add_lp(tx, outbox, owner, CORP_HERALDRY, -amount, mutation).await.map_err(Error::from)?;
                let receipt = PaymentRow { goal_id: id.clone(), character_id: owner, day_start: actual.period_start, ordinal: i16::try_from(paid + 1).map_err(|_| Error::Invalid)?, issuer_id: CORP_HERALDRY, amount, paid_at: at };
                if !receipts::insert_payment(tx, &receipt).await? { return Err(TxError::Stale); }
                let change = batch.paid().ok_or(Error::Unavailable)?;
                change.prelock(tx).await?;
                change.apply(tx, outbox).await?;
                Ok(Outcome::Paid)
            })
        }, |value, outbox| async move {
            if let Err(error) = outbox::dispatch_world((), outbox, &resources.world) { tracing::warn!(?error, "daily payment post-commit dispatch failed"); }
            Ok(value)
        }).await;
        match result { Ok(Outcome::Paid) => return Ok(()), Ok(Outcome::Natural) => return Err(Error::Forbidden), Err(TxError::Stale) => continue, Err(TxError::Domain(error)) => return Err(error) }
    }
}

#[cfg(test)] mod tests;
