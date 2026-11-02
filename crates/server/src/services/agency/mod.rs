//! Agency and corporation service routing for `fwCharacterEnlistmentMgr`
//! and `roguelikeMgr`, including enlistment client calls.
//! Reads game corporation whitelists through its service leaves.
//! Roguelike runs are not modelled.

mod fw_character_enlistment_mgr;
mod roguelike_mgr;

use super::{Call, CallResult};
use crate::client::Client;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "fwCharacterEnlistmentMgr" => fw_character_enlistment_mgr::dispatch(c, call).await,
        "roguelikeMgr" => roguelike_mgr::dispatch(c, call),
        _ => None,
    }
}
