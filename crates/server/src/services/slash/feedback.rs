//! Formats slash results for the client's private system conversation.
//! `svc.slash` still receives the original RPC result; this reads no data store
//! and drops replies when the selected character is offline.

use std::borrow::Cow;
use super::{CallError, CallResult, Client, Value};
use infinity_core::messages;

fn text(result: &CallResult) -> Cow<'_, str> {
    match result {
        Ok(Value::Unicode(text)) => Cow::Borrowed(text),
        Ok(Value::Int(id)) => Cow::Owned(messages::gm::gm_spawn_feedback(*id)),
        Err(CallError::User { msg, dict }) => {
            let key = if msg == messages::MSG_SLASH_ERROR { Some("reason") }
                else if msg == messages::MSG_CUSTOM_NOTIFY { Some("notify") }
                else { None };
            key.and_then(|key| dict.iter().find_map(|(name, value)| match value {
                Value::Unicode(text) if name == key => Some(text.as_str()),
                _ => None,
            })).map(Cow::Borrowed).unwrap_or(Cow::Borrowed(messages::gm::GM_FEEDBACK_ERROR))
        }
        _ => Cow::Borrowed(messages::gm::GM_FEEDBACK_ERROR),
    }
}

pub(super) fn publish(c: &Client, result: &CallResult) {
    if let Some(char_id) = c.session.int("charid") {
        c.chat.send_system_private(char_id, &text(result));
    }
}
