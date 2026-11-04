//! Crate service routing for eve/client/script/ui/crate/controller.py and
//! recruitment_crate_controller.py. Previews use the startup catalogue and
//! authenticated inventory; round-one direct-list opening is transactional.

mod catalogue;
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
