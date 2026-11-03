//! Ship dirt timestamp service; no persistent dirt state is modelled.
//! Special crate fitting metadata is unavailable and is refused.
use super::*;
pub(super) async fn dispatch(_c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetDirtTimestamp" => Some(Ok(Value::Int(crate::client::filetime_now()))),
        "GetShipFittingInfo" => Some(Err(crate::services::notify_error(crate::messages::crates::METADATA_UNAVAILABLE))),
        _ => None,
    }
}
