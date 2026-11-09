//! Shared slash test helpers: the real RPC route, wire encoding and the
//! production online-pilot registry.
use super::*;
use infinity_core::session::OnlineSession;
use tokio::sync::mpsc::UnboundedReceiver;

/// Send one `/...` line through the real dispatch chain the way `slash.SlashCmd`
/// receives it: a single unicode argument and empty kwargs.
pub(in crate::services::slash) async fn rpc(c: &mut Client, line: &str) -> CallResult {
    let args = [Value::unicode(line)];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "slash", method: "SlashCmd", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(c, &call).await
}

/// Encode a result the way the wire receives it: the returned value, or the
/// UserError tuple the client raises.
pub(in crate::services::slash) fn wire(result: CallResult) -> Vec<u8> {
    let value = match result {
        Ok(value) => value,
        Err(CallError::User { msg, dict }) => crate::client::user_error(&msg, dict),
        Err(CallError::Internal(error)) => panic!("unexpected internal error: {error}"),
    };
    marshal::dumps(&value)
}

/// Register a live notification channel and its online snapshot through the
/// production registry; the receiver observes everything the pilot would get.
pub(in crate::services::slash) fn register_online(
    world: &crate::space::World,
    session: OnlineSession,
) -> UnboundedReceiver<(String, Vec<Value>)> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(session.character_id, tx);
    world.set_online_session(session.character_id, Some(session));
    rx
}
