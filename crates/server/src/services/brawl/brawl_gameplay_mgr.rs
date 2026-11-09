//! Closed gameplay and recovery consumed by `brawl/client/brawl_svc.py` and its HUD.
//! No data source is used; fight instances, ships and gameplay are not implemented.

use super::super::{notify_error, Call, CallResult};
use infinity_core::messages;
use crate::client::Client;
use marshal::Value;

/// OUTSIDE from `eveProto/generated/eve_public/brawl/brawl_pb2.py:37-45`.
const BRAWL_STATUS_OUTSIDE: i64 = 1;

pub(super) fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("brawlGameplayMgr", "FetchCharacterData") => {
            c.notify("OnServerBrawlStatusNotified", vec![Value::Int(BRAWL_STATUS_OUTSIDE)]);
            Some(Ok(Value::None))
        }
        ("brawlGameplayMgr", "DropCloud" | "AutoEquip" | "ActiveEntropyModule" | "UpgradeEntropyModule" | "UnlockEntropyModule" | "ReleaseCloud" | "CollectCloud" | "TakeKillingMedal") =>
            Some(Err(notify_error(messages::brawl::UNAVAILABLE))),
        _ => None,
    }
}
