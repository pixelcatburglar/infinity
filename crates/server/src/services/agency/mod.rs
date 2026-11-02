//! Agency and corporation service routing for `agentMgr`, `fwCharacterEnlistmentMgr`,
//! and `roguelikeMgr`, including epic arc and enlistment client calls.
//! Reads SDE missions and game corporation whitelists through its service leaves.
//! Roguelike runs are not modelled.

mod agent_mgr;
mod fw_character_enlistment_mgr;
mod roguelike_mgr;

use super::{Call, CallResult};
use crate::client::Client;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "agentMgr" => agent_mgr::dispatch(c, call).await,
        "fwCharacterEnlistmentMgr" => fw_character_enlistment_mgr::dispatch(c, call).await,
        "roguelikeMgr" => roguelike_mgr::dispatch(c, call),
        _ => None,
    }
}
