//! Routes closed brawl compatibility for the client brawl services.
//! `brawl/client/brawl_svc.py` consumes recovery status; no data source is used.
//! Matches, instances and brawl gameplay are not implemented.

mod brawl_mgr;
mod brawl_gameplay_mgr;
mod brawl_new_player_mgr;

use super::{Call, CallResult};
use crate::client::Client;

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    brawl_mgr::dispatch(call)
        .or_else(|| brawl_new_player_mgr::dispatch(call))
        .or_else(|| brawl_gameplay_mgr::dispatch(c, call))
}
