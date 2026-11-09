//! Drone commands follow the client entity moniker and ship service contracts.
//! Michelle receives drone state through bootstrap and state notifications; it
//! does not issue a remote GetDroneState call. Salvage and reconnect are refused.

pub(crate) mod inventory;
pub(crate) mod mining;
pub(crate) mod worker;
pub(crate) mod settings;
mod entity;

use crate::client::Client;
use crate::services::{Call, CallResult};

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service == "entity" { Some(entity::dispatch(c, call).await) } else { None }
}

#[cfg(test)]
pub(crate) mod tests;
