//! Player bounty pools, kill rights and their shared optional-ID wire encoding.
//! Mirrors eve/client/script/ui/shared/bountySvc.py and killRights.py.
//! Uses game.bounties, game.bounty_contributions, game.bounty_payouts and game.kill_rights.
//! GM cache/reimbursement calls and type-14 notifications are unsupported. Active
//! kill-right suspect flags are restored at startup; other Crimewatch flags are memory-only.

mod bounty_proxy;
mod bounties;
mod kill_right_mgr;
pub(super) mod kill_rights;
pub(super) mod payout;
pub(crate) mod npc;
#[cfg(all(test, feature = "tests-combat"))] mod tests;

use super::{Call, CallResult};
use crate::client::Client;
use crate::{db::Db, space::World};
use std::sync::Arc;

/// Restore each active right's stored deadline; the count is the number of rights.
pub(crate) async fn restore_activations(db: &Db, world: &Arc<World>) -> Result<usize, super::CallError> {
    let rights = crate::dao::kill_rights::active_activations(db, crate::client::filetime_now()).await.map_err(super::db_err)?;
    let count = rights.len();
    for right in rights {
        if let Some(until) = right.activated_until {
            super::crimewatch::flag_suspect(db, world, right.to_id, until).await;
        }
    }
    Ok(count)
}

/// Client appConst.py minimum player bounty.
const MIN_CHARACTER_BOUNTY: f64 = 1_000_000.0;
/// Client appConst.py minimum corporation bounty.
const MIN_CORPORATION_BOUNTY: f64 = 20_000_000.0;
/// Client appConst.py minimum alliance bounty.
const MIN_ALLIANCE_BOUNTY: f64 = 100_000_000.0;
/// Client appConst.py maximum bounty pool.
const MAX_BOUNTY: f64 = 100_000_000_000.0;
/// Server policy for leaderboard pages.
const TOP_LIST_PAGE: i64 = 50;
/// Server policy for search results on either side of the matching rank.
const SEARCH_CONTEXT: i64 = 5;

fn optional(id: Option<i64>) -> marshal::Value { id.map(marshal::Value::Int).unwrap_or(marshal::Value::None) }

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "bountyProxy" => bounty_proxy::dispatch(c, call).await,
        "killRightMgr" => kill_right_mgr::dispatch(c, call).await,
        _ => None,
    }
}
