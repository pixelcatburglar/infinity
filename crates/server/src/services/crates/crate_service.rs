//! crateService RPCs used by eve/client/script/ui/crate/controller.py and
//! recruitment_crate_controller.py. The SDE catalogue supplies preview data;
//! instance previews also consult authenticated inventory. Opening, location
//! lookup and reward claims are refused and do not mutate state.

use super::catalogue;
use crate::client::Client;
use crate::messages::crates as messages;
use crate::services::{notify_error, Call, CallResult};
use crate::services::identity::Identity;
use marshal::Value;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let id = || match call.args.first() {
        Some(Value::Int(id)) if *id > 0 => Some(*id),
        _ => None,
    };
    Some(match call.method {
        "GetCrate" => match id().and_then(|id| i32::try_from(id).ok()) {
            Some(_) => Ok(Value::list([])),
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "PeekInsideCrate" => match id().and_then(|id| i32::try_from(id).ok()) {
            Some(type_id) => catalogue::directory(c, type_id, false, &Identity::of_client(c)).await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "CustomGetLootFromFixedCrate" | "PeekInsideFixedCrate" => match id() {
            Some(item_id) => catalogue::instance(c, item_id, call.method == "CustomGetLootFromFixedCrate").await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "GetCrateLocation" | "OpenCrate" | "ClaimLootFromCrateStack" | "CustomOpenCrate"
        | "ClaimLoot" | "ClaimSkillBundle" | "ClaimFitting" => Err(notify_error(messages::OPENING_UNAVAILABLE)),
        _ => return None,
    })
}
