//! Durable player-bounty payouts for killmails, using game.bounties and game.bounty_payouts.
//! Mirrors the client bounty payout event; NPC bounties are handled elsewhere.
use crate::{client::filetime_now, dao::bounties as dao, db::{Db, executor::Executor, outbox::{dispatch_world, Outbox}}, scheduler::{self, Job}, space::World};
use crate::services::{db_err, market, wallets, CallError};
use marshal::Value;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};

/// Server policy: cap player-bounty payouts at 20% of the victim's destroyed value; this anti-farming cap limits self-funded bounty loops while keeping payouts tied to the loss.
const PAYOUT_FRACTION: f64 = 0.2;
/// Wallet and bounty storage use two decimal places.
const CENTS_PER_ISK: i64 = 100;
/// Client notificationconst bounty payout and bounty group.
const PAYOUT_NOTIFICATION: i32 = 111;
const BOUNTY_GROUP: i32 = 10;
pub(in crate::services) const KIND: &str = "bounty-payout";

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::services) struct Payload {
    pub killmail_id: i64,
    pub victim: i64,
    pub victim_corp: i64,
    pub ship_type: i32,
    pub final_char: i64,
    pub isk_lost: f64,
}

/// Apportion cents by largest remainder, with ties resolved by ascending target ID.
pub(super) fn shares(pools: &[(i64, i64)], payout: i64) -> Vec<i64> {
    let total: i64 = pools.iter().map(|(_, amount)| *amount).sum();
    if total == 0 || payout == 0 { return vec![0; pools.len()]; }
    let mut shares = Vec::with_capacity(pools.len());
    let mut remainders = Vec::with_capacity(pools.len());
    for (index, (id, amount)) in pools.iter().enumerate() {
        let product = i128::from(payout) * i128::from(*amount);
        shares.push((product / i128::from(total)) as i64);
        remainders.push((product % i128::from(total), *id, index));
    }
    let missing = payout - shares.iter().sum::<i64>();
    remainders.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, _, index) in remainders.into_iter().take(missing as usize) { shares[index] += 1; }
    shares
}

pub(in crate::services) async fn schedule(ex: &(impl Executor + ?Sized), payload: Payload, now: i64) -> Result<(), CallError> {
    let job = Job { id: format!("{KIND}:{}", payload.killmail_id), kind: KIND.into(), due_at: now,
        payload: serde_json::to_string(&payload).map_err(db_err)? };
    scheduler::schedule(ex, &job).await.map_err(db_err)?;
    Ok(())
}

pub(in crate::services) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Payload = serde_json::from_str(&job.payload)?;
        let _lock = market::lock().await;
        let now = filetime_now();
        let result = db.transaction(|tx, outbox: &mut Outbox| {
            let payload = payload.clone();
            Box::pin(async move {
                let victim_row = dao::get(tx, &[payload.victim]).await.map_err(db_err)?;
                let mut targets = vec![payload.victim, payload.victim_corp];
                if let Some(alliance) = victim_row[0].alliance_id { targets.push(alliance); }
                targets.sort_unstable(); targets.dedup();
                let pools = dao::pools(tx, &targets).await.map_err(db_err)?;
                let available: i64 = pools.iter().map(|pool| pool.cents).sum();
                let cents = dao::payout_limit(tx, available, payload.isk_lost, PAYOUT_FRACTION).await.map_err(db_err)?;
                if !dao::record_payout(tx, payload.killmail_id, payload.final_char, cents, now).await.map_err(db_err)? { return Ok(()); }
                if cents <= 0 { return Ok(()); }
                let allocations = shares(&pools.iter().map(|pool| (pool.target_id, pool.cents)).collect::<Vec<_>>(), cents);
                let mut debited = Vec::new();
                for (pool, share) in pools.iter().zip(allocations) {
                    if share <= 0 { continue; }
                    if !dao::debit(tx, pool.target_id, share).await.map_err(db_err)? {
                        return Err(CallError::Internal(anyhow::anyhow!("bounty pool changed during payout")));
                    }
                    debited.push(pool.target_id);
                }
                let amount = cents as f64 / CENTS_PER_ISK as f64;
                wallets::credit(tx, outbox, payload.final_char, crate::isk::IskAmount::from_cents(i128::from(cents)).ok_or(crate::isk::AmountError)?).await?;
                outbox.notify_pilot(payload.final_char, "OnBountyAddedToPayout", vec![Value::dict([
                    ("amount", Value::Float(amount)), ("payoutTime", Value::Int(now)),
                    ("enemyTypeID", Value::Int(payload.ship_type as i64)), ("isModified", Value::Bool(false)),
                ])]);
                let data = format!("{{victimID: {}, bounty: {amount}}}", payload.victim);
                for placer in dao::placers(tx, &debited).await.map_err(db_err)? {
                    super::super::mail::notify(tx, outbox, placer, PAYOUT_NOTIFICATION, BOUNTY_GROUP, payload.final_char, &data).await?;
                }
                Ok(())
            })
        }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await;
        result.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("bounty payout refused: {msg}"),
        })
    })
}
