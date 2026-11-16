//! Mutaplasmid RPC routing and atomic mutation mechanics for
//! `eve/client/script/dogma/dynamicItemSvc.py`. Static ranges come from the SDE;
//! provenance and absolute bases are persisted in game tables. Roguelike variants,
//! renewal and erase are unsupported. Deployed sources require World removal and
//! cannot be mutated by this inventory-only service.
mod dynamic_item_service;
mod mutation;

use crate::client::Client;
use super::{Call, CallResult};

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "dynamicItemService" { return None; }
    dynamic_item_service::dispatch(c, call).await
}

#[cfg(all(test, feature = "tests-effects"))]
mod tests;
