//! Peace treaty session results for `peaceTreatyMgr`, used by `war_cso.py`.
//! Mutual retraction reads and changes `game.wars` but creates no forced treaty.
//! Persisted treaties and non-mutual peace negotiations are unsupported.

use crate::client::Client;
use crate::services::{Call, CallResult, notify_error};
use infinity_core::messages;
use marshal::Value;

pub(super) async fn dispatch(_c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetPeaceTreatiesForSession" if call.args.is_empty() => Some(Ok(Value::tuple([Value::list([]), Value::list([])]))),
        "GetPeaceTreatiesForSession" => Some(Err(notify_error(messages::wars::INVALID))),
        _ => None,
    }
}
