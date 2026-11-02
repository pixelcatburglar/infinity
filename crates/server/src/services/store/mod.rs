//! New Eden Store, PLEX vault, redeeming queue and seasonal login rewards.
//!
//! * `storeManager` (vgs/client/vgsClient.py): the catalogue as the JSON-shaped
//!   dicts vgs/common/utils.py parses. The live catalogue was server-side only, so
//!   it is rebuilt from this build's data: Serenity-visible SKINs, apparel and
//!   crates that traded on the retail market, priced in PLEX at the retail
//!   ISK/PLEX ratio (crate::retail), plus skill services and a free daily gift.
//!   Category ids are the ones the store UI has icons for (nes_constants.py).
//! * `vaultManager`: the account's PLEX.
//! * `userSvc` redeem tokens (eve/client/script/ui/services/redeemsvc.py): what
//!   purchases and rewards deliver; claiming turns them into items, or free skill
//!   points for the skill-point types the client lists in redeem/data.py.
//! * `seasonalLoginCampaignManager` (loginrewards/client/seasonalLoginCampaignService.py):
//!   the newest seasonal campaign in seasonalCampaignsLoginRewards, one claim per day.
//!
//! Account PLEX is stored exactly up to 36 digits in `game.accounts`; the DAO
//! clamps its client-facing i64 projection. Balance notifications include `userid`.
//! Purchases and redeeming use `game.store_purchases` and `game.redeem_tokens`.
//! Clients cannot display oversized balances exactly.
//! Daily gift, turnover and checkout policies come from the game configuration.

use super::{db_err, keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, unix_seconds_from_filetime, Client};
use crate::consts::FLAG_HANGAR;
use crate::messages;
use marshal::Value;
use tracing::info;


fn s(v: &str) -> Value {
    Value::unicode(v)
}


/// `YYYY-MM-DDTHH:MM:SSZ` of the current daily period's start.
fn period_start() -> String {
    period_start_at(unix_seconds_from_filetime(filetime_now()), &crate::config::game().store.daily_turnover_utc)
}

fn period_start_at(now: i64, turnover: &str) -> String {
    let mut parts = turnover.split(':').map(|part| part.parse::<i64>().expect("validated UTC turnover"));
    let offset = parts.next().unwrap() * crate::consts::SECONDS_PER_HOUR
        + parts.next().unwrap() * crate::consts::SECONDS_PER_MINUTE + parts.next().unwrap();
    let day = (now - offset).div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{turnover}Z")
}


// ---------------------------------------------------------------- PLEX and redeeming

pub(super) fn notify_plex_balance(c: &mut Client, balance: i64) {
    c.notify("OnAurumChangeFromVgs", vec![Value::dict([
        ("userid", Value::Int(c.login.user_id)),
        ("balance", Value::Int(balance)),
    ])]);
}

/// Queue `quantity` x `type_id` on `user_id`'s redeeming queue.
async fn grant_token(c: &Client, user_id: i64, type_id: i32, quantity: i64, label: &str) -> Result<(), CallError> {
    let quantity = i32::try_from(quantity).map_err(|e| db_err(anyhow::anyhow!("redeem quantity out of range: {e}")))?;
    crate::dao::store::purchases::grant(&c.db, user_id, type_id, quantity, label, filetime_now()).await.map_err(db_err)?;
    Ok(())
}




mod store_manager;
mod vault_manager;
mod user_svc;
mod fast_checkout_service;
mod seasonal_login_campaign_manager;
mod login_reward_facilities;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "storeManager" => store_manager::dispatch(c, call).await,
        "vaultManager" => vault_manager::dispatch(c, call).await,
        "userSvc" => user_svc::dispatch(c, call).await,
        "FastCheckoutService" => fast_checkout_service::dispatch(c, call).await,
        "seasonalLoginCampaignManager" => seasonal_login_campaign_manager::dispatch(c, call).await,
        "loginRewardFacilities" => login_reward_facilities::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
