//! Chat domain admission and durable room authority for `xmppchatclient/xmppchatsvc.py`
//! and `xmppchannelsettings.py`, using session facts, the SDE map and
//! `game.chat_channels`, `game.chat_channel_members` and `game.chat_system_motds`.
//! Occupants and messages remain runtime transport state. Attaching a deadline
//! normalizes the matching plain restriction into a timed record; thereafter plain
//! restrictions and unexpired records apply independently. Scheduler settlement
//! and removal clear only the record, preserving later plain restrictions.
//! Passwords, corporation/alliance ACL grants, proxy-room ownership and
//! client-preference nullsec blackout are unsupported.
pub(crate) mod authority;
pub(crate) mod expiry;
mod xmpp_chat_mgr;

use super::{Call, CallResult};
use crate::client::Client;

pub(crate) const PERMISSIONS_REFRESH: &str = "__chat_permissions";

pub(super) async fn root(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "XmppChatMgr" { xmpp_chat_mgr::dispatch(c, call).await } else { None }
}

#[cfg(all(test, feature = "tests-social"))] mod tests;
