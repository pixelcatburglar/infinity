//! Closed match operations consumed by `brawl/client/brawl_controller.py`.
//! No data source is used; matchmaking and fights are not implemented.

use super::super::{notify_error, Call, CallResult};
use infinity_core::messages;

pub(super) fn dispatch(call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("brawlMgr", "CancelMatch" | "LaunchMatch" | "LeaveFight" | "ChooseSpawnDungeon" | "launch_match" | "StartNewPlayerFight") =>
            Some(Err(notify_error(messages::brawl::UNAVAILABLE))),
        _ => None,
    }
}
