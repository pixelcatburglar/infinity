//! seasonalLoginCampaignManager for loginrewards/client/seasonalLoginCampaignService.py.
//! Uses SDE campaign data and game reward claims; only the newest campaign is supported.
use super::*;
use crate::{db::{Db, outbox::{dispatch_world, Outbox}}, sde::store::Campaign, services::subscriptions::rules, space::{NotifyTx, World}};
use std::sync::Arc;

/// Settle an account's new daily reward and any eligible missed Omega rewards atomically.
async fn claim_reward(db: &Db, world: &World, campaign: &Campaign, user: i64, sender: NotifyTx) -> Result<bool, CallError> {
    let _lock = crate::services::market::lock().await;
    let now = filetime_now();
    let data = Arc::new((campaign.id, campaign.alpha.clone(), campaign.omega.clone(), campaign.retro_claim));
    db.transaction(|tx, outbox: &mut Outbox| {
        let data = Arc::clone(&data);
        let sender = sender.clone();
        Box::pin(async move {
            let (campaign, alpha, omega, retro) = &*data;
            let authority = crate::dao::accounts::subscriptions::lock(tx, user).await.map_err(db_err)?;
            let grade = rules::effective_grade(&authority, now);
            let (claimed, today) = crate::dao::store::rewards::claims(tx, user, *campaign).await.map_err(db_err)?;
            let history = crate::dao::store::rewards::history(tx, user, *campaign).await.map_err(db_err)?;
            let mut changed = false;
            if *retro && grade == rules::OMEGA {
                for (day, row) in history.iter().enumerate() {
                    if row.clone_grade != i32::from(rules::ALPHA) { continue; }
                    let &(type_id, quantity) = omega.get(day).ok_or_else(|| db_err(anyhow::anyhow!("seasonal history exceeds campaign rewards")))?;
                    let quantity = i32::try_from(quantity).map_err(db_err)?;
                    if !crate::dao::store::rewards::mark_omega(tx, user, *campaign, row.ordinal).await.map_err(db_err)? {
                        return Err(db_err(anyhow::anyhow!("seasonal history changed during settlement")));
                    }
                    crate::dao::store::purchases::grant(tx, user, type_id, i64::from(quantity), "UI/LoginRewards/WindowCaption", now).await.map_err(db_err)?;
                    crate::dao::store::rewards::audit_retro(tx, user, *campaign, row.ordinal, type_id, quantity, now).await.map_err(db_err)?;
                    changed = true;
                }
            }
            let total = alpha.len().min(omega.len());
            if !today && (claimed as usize) < total {
                let day = claimed as usize;
                if crate::dao::store::rewards::claim(tx, user, *campaign, i32::from(grade)).await.map_err(db_err)? != 1 {
                    return Err(db_err(anyhow::anyhow!("seasonal claim was not inserted")));
                }
                for &(type_id, quantity) in std::iter::once(&alpha[day]).chain((grade == rules::OMEGA).then_some(&omega[day])) {
                    crate::dao::store::purchases::grant(tx, user, type_id, quantity, "UI/LoginRewards/WindowCaption", now).await.map_err(db_err)?;
                }
                changed = true;
            }
            if changed { outbox.notify_sender(sender, "OnRedeemingQueueUpdated", vec![]); }
            Ok(changed)
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, world) }).await
}

/// The newest seasonal campaign: (id, alpha rewards, omega rewards), each reward `(typeID, quantity)` by day.
fn seasonal_campaign(c: &Client) -> Option<&crate::sde::store::Campaign> { c.sde.store.campaign() }
/// (claims so far, claimed today) for `campaign`.
async fn seasonal_claims(c: &Client, campaign: i32) -> Result<(i64, bool), CallError> {
    crate::dao::store::rewards::claims(&c.db, c.login.user_id, campaign).await.map_err(db_err)
}

async fn seasonal(c: &mut Client, call: &Call<'_>) -> CallResult {
    let Some(data) = seasonal_campaign(c) else {
        return Ok(match call.method {
            "get_active_campaign" => Value::tuple([Value::None, Value::None, Value::None, Value::None]),
            "get_claim_history" => Value::Dict(vec![]),
            _ => Value::Bool(false),
        });
    };
    let campaign = data.id;
    let total = data.alpha.len().min(data.omega.len()) as i64;
    match call.method {
        // (campaignID, end date ordinal, daily turnover, rewards remaining). The campaign stays
        // open until every reward is claimed: it ends the day after the last one would be.
        "get_active_campaign" => {
            let (claimed, _) = seasonal_claims(c, campaign).await?;
            let remaining = total - claimed;
            let today_ordinal = crate::dao::store::rewards::today_ordinal(&c.db).await.map_err(db_err)?;
            let end = today_ordinal as i64 + remaining + 1;
            Ok(Value::tuple([Value::Int(campaign as i64), Value::Int(end), Value::str(&crate::config::game().store.daily_turnover_utc), Value::Int(remaining)]))
        }
        // {date ordinal: clone grade}
        "get_claim_history" => {
            let rows = crate::dao::store::rewards::history(&c.db, c.login.user_id, campaign).await.map_err(db_err)?;
            Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.ordinal as i64), Value::Int(r.clone_grade as i64))).collect()))
        }
        "claim_reward" => {
            let claimed = claim_reward(&c.db, &c.world, data, c.login.user_id, c.notify_tx.clone()).await?;
            Ok(Value::Bool(claimed))
        }
        _ => Ok(Value::None),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("seasonalLoginCampaignManager", _) => seasonal(c, call).await,

        _ => return None,
    })
}


#[cfg(test)] mod tests;
