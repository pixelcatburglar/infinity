//! Crate service routing for eve/client/script/ui/crate/controller.py and
//! recruitment_crate_controller.py. Previews use the startup catalogue and
//! authenticated inventory; opening and claiming rewards are not supported.

mod catalogue;
mod crate_service;

use crate::client::Client;
use crate::services::{Call, CallResult};

pub(crate) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "crateService" => crate_service::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
