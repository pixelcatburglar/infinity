//! Durable recovery deadlines in `game.jobs`. The client consumes recovery
//! notifications through `repairableShip/client/repairable_ship_svc.py`; jobs
//! share kind/member IDs and carry immutable identities and deadlines. No timer is held
//! in memory, and stale or repeated jobs never change a newer member state.
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use serde::{Deserialize, Serialize};
use crate::{client::filetime_now, dao::repairable_ship::{self as dao, RecoveryItemId, Status}, db::{outbox::{dispatch_world, Outbox}, Db}, scheduler::{self, Job}, services::{db_err, market, CallError}, space::World};
use super::recovery;

pub(crate) const PAYMENT: &str = "repairable.payment.expiry";
pub(crate) const DELIVERY: &str = "repairable.delivery.arrival";
pub(crate) const WITHDRAW: &str = "repairable.withdrawal.expiry";

#[derive(Serialize, Deserialize)]
struct Payload { member: i64, deadline: i64 }
fn job(kind: &'static str, member: RecoveryItemId, deadline: i64) -> Job {
    Job { id: job_id(kind, member), kind: kind.into(), due_at: deadline,
        payload: serde_json::json!({"member":member.get(), "deadline":deadline}).to_string() }
}
pub(crate) fn payment_job(member: RecoveryItemId, deadline: i64) -> Job { job(PAYMENT, member, deadline) }
pub(super) fn delivery_job(member: RecoveryItemId, deadline: i64) -> Job { job(DELIVERY, member, deadline) }
pub(super) fn withdraw_job(member: RecoveryItemId, deadline: i64) -> Job { job(WITHDRAW, member, deadline) }
/// Stable scheduler identity for one recovery kind and member.
pub(super) fn job_id(kind: &str, member: RecoveryItemId) -> String { format!("{kind}:{}", member.get()) }

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Payload = serde_json::from_str(&job.payload)?;
        let Some(member) = RecoveryItemId::new(payload.member) else { return Ok(()) };
        if ![PAYMENT, DELIVERY, WITHDRAW].contains(&job.kind.as_str()) || job.id != job_id(&job.kind, member) { return Ok(()); }
        let deadline = payload.deadline;
        let _guard = market::lock().await;
        let sde = world.sde().clone();
        let kind = job.kind;
        db.transaction(|tx, _outbox: &mut Outbox| {
            let sde = sde.clone();
            let kind = kind.clone();
            Box::pin(async move {
                let Some(row) = dao::get_member(tx, member).await.map_err(db_err)? else { return Ok(()) };
                let Some(group) = dao::get_group(tx, row.group).await.map_err(db_err)? else { return Ok(()) };
                let requested = BTreeMap::from([(group.id, vec![member])]);
                let selections = dao::load_selection(tx, group.beneficiary, &requested, true).await.map_err(db_err)?;
                let Some(selection) = selections.first() else { return Ok(()) };
                let Some(row) = selection.items.first() else { return Ok(()) };
                let now = filetime_now();
                if now < deadline { return Ok(()); }
                match kind.as_str() {
                    PAYMENT if row.status == Status::AwaitingPayment && selection.group.payment_expires_at == deadline => {
                        recovery::discard_members(tx, &sde, &selection.group, std::slice::from_ref(row), &[Status::AwaitingPayment], Status::Expired, now).await?;
                    }
                    DELIVERY if row.status == Status::Delivering && row.delivery_completed_at == Some(deadline) => {
                        if dao::mark_ready(tx, &[member], now).await.map_err(db_err)?.len() != 1 { return Err(CallError::Internal(anyhow::anyhow!("recovery arrival changed"))); }
                    }
                    WITHDRAW if matches!(row.status, Status::Delivering | Status::Ready) && row.withdraw_expires_at == Some(deadline) => {
                        recovery::discard_members(tx, &sde, &selection.group, std::slice::from_ref(row), &[Status::Delivering, Status::Ready], Status::Expired, now).await?;
                        scheduler::cancel(tx, &job_id(DELIVERY, member)).await.map_err(db_err)?;
                    }
                    _ => {}
                }
                Ok(())
            })
        }, |value, outbox| async move { let result = dispatch_world(value, outbox, &world); scheduler::wake(); result }).await.map_err(|error| anyhow::anyhow!("recovery deadline failed: {error:?}"))
    })
}

#[cfg(all(test, feature = "tests-services"))] mod tests;
