//! Shared slash test helpers: the real RPC route, wire encoding and the
//! production online-pilot registry.
#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-content", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash", feature = "tests-social"))]
use crate::{client::Client, services::{Call, CallResult}};
#[cfg(any(feature = "tests-combat", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash"))]
use crate::services::CallError;
#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-content", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash", feature = "tests-social"))]
use marshal::Value;
#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash", feature = "tests-social"))]
use infinity_core::session::OnlineSession;
#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash", feature = "tests-social"))]
use tokio::sync::mpsc::UnboundedReceiver;

#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-content", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash", feature = "tests-social"))]
/// Send one `/...` line through the real dispatch chain the way `slash.SlashCmd`
/// receives it: a single unicode argument and empty kwargs.
pub(in crate::services) async fn rpc(c: &mut Client, line: &str) -> CallResult {
    let args = [Value::unicode(line)];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(c, &call).await
}

#[cfg(any(feature = "tests-combat", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash"))]
/// Encode a result the way the wire receives it: the returned value, or the
/// UserError tuple the client raises.
pub(in crate::services) fn wire(result: CallResult) -> Vec<u8> {
    let value = match result {
        Ok(value) => value,
        Err(CallError::User { msg, dict }) => crate::client::user_error(&msg, dict),
        Err(CallError::Internal(error)) => panic!("unexpected internal error: {error}"),
    };
    marshal::dumps(&value)
}

#[cfg(any(feature = "tests-combat", feature = "tests-effects", feature = "tests-inventory", feature = "tests-ships", feature = "tests-slash", feature = "tests-social"))]
/// Register a live notification channel and its online snapshot through the
/// production registry; the receiver observes everything the pilot would get.
pub(in crate::services) fn register_online(
    world: &crate::space::World,
    session: OnlineSession,
) -> UnboundedReceiver<(String, Vec<Value>)> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(session.character_id, tx);
    world.set_online_session(session.character_id, Some(session));
    rx
}
