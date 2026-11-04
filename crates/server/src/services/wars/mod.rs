//! Mutual wars, persistent invitations, reports and deadlines, mirroring
//! `eve/client/script/ui/services/wars/war_cso.py` and `eve/client/script/ui/services/wars/mutualWarInvites.py`.
//! Uses `game.wars`, `game.war_invites`, `game.war_invite_settings`, `game.jobs`,
//! `game.killmail_wars`, killmail snapshots and startup SDE data.
//! Aggressive wars, allies and paid surrender or treaties are unsupported.
pub(crate) mod authority;
pub(crate) mod classification;
pub(crate) mod statistics;
pub(super) mod deadlines;
mod lifecycle;
mod peace_treaty_mgr;
mod mutual_war_invite_mgr;
mod war_registry;
mod wars_info_mgr;
mod wire;
use crate::{client::Client, services::{Call, CallResult}};

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "mutualWarInviteMgr" => mutual_war_invite_mgr::dispatch(c, call).await,
        "warsInfoMgr" => wars_info_mgr::dispatch(c, call).await,
        "warRegistry" => war_registry::dispatch(c, call).await,
        "peaceTreatyMgr" => peace_treaty_mgr::dispatch(c, call).await,
        _ => None,
    }
}
