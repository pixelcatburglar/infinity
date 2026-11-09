//! crateService RPCs used by eve/client/script/ui/crate/controller.py and
//! recruitment_crate_controller.py. The SDE catalogue supplies preview data;
//! instance previews consult authenticated inventory. Destination queries and
//! opening support ordinary direct-list definitions; custom and special claims remain refused.

use super::catalogue;
use crate::client::Client;
use infinity_core::messages::crates as messages;
use crate::services::{notify_error, Call, CallResult};
use crate::services::identity::Identity;
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let identity = Identity::of_client(c);
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
            Some(type_id) => catalogue::directory(c, type_id, false, &identity).await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "CustomGetLootFromFixedCrate" | "PeekInsideFixedCrate" => match id() {
            Some(item_id) => catalogue::instance(c, item_id, call.method == "CustomGetLootFromFixedCrate").await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "GetCrateLocation" => match id() {
            Some(source) => super::opening::location(c, source).await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "OpenCrate" => match id() {
            Some(source) => super::opening::open(c, source, 1, false).await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "ClaimLootFromCrateStack" => match (id(), call.args.get(1)) {
            (Some(source), Some(Value::Int(amount))) => match i32::try_from(*amount) {
                Ok(amount) => super::opening::open(c, source, amount, true).await,
                Err(_) => Err(notify_error(messages::INVALID_SOURCE)),
            },
            _ => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "ClaimLoot" => match super::claims::arguments(call) {
            Some((item, destination, flag)) => super::claims::claim_loot(c, item, destination, flag).await,
            None => Err(notify_error(messages::INVALID_SOURCE)),
        },
        "CustomOpenCrate" | "ClaimSkillBundle" | "ClaimFitting" => Err(notify_error(messages::OPENING_UNAVAILABLE)),
        _ => return None,
    })
}
