//! Crate service routing for eve/client/script/ui/crate/controller.py and
//! recruitment_crate_controller.py. Previews use the startup SDE catalogue,
//! composed from native crate data and server-authored content, and instance
//! previews check authenticated inventory access.
//!
//! Destination queries apply the same authorization and eligibility checks as
//! opening, without reserving anything; opening checks the current state again.
//! Authorized, reachable ordinary direct-list crates are opened transactionally:
//! source quantities are consumed, rewards are created at the destination, and
//! durable receipts are recorded in `game.crate_rewards`. `ClaimLoot` only
//! acknowledges a matching receipt; it neither creates nor moves items.
//! Custom opening and special rewards such as skill bundles and fittings are
//! unsupported.

mod catalogue;
mod claims;
mod crate_service;
mod opening;

use crate::client::Client;
use crate::services::{Call, CallResult};

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "crateService" => crate_service::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
