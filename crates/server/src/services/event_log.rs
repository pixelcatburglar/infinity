//! Crash delivery from clientStatsSvc.py through eventLog into game.client_crashes.
//! LogClientStats stores crashes; LogPlayerRequestedDisconnect and LogClientEvent
//! are acknowledged without storage. Other methods are unsupported. Calls with
//! more than 1024 crashes or a crash Debug representation over 64 KiB are rejected
//! whole, with formatting bounded before any crash is stored.
use crate::{client::Client, dao::client_errors, services::{Call, CallResult, db_err}};
use marshal::Value;
use std::fmt::{self, Write};

/// Server policy: bound per-call crash persistence work.
const MAX_CRASHES: usize = 1024;
/// Server policy: bound each persisted crash representation.
const MAX_CRASH_BYTES: usize = 64 * 1024;

struct CrashText(String);

impl fmt::Write for CrashText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > MAX_CRASH_BYTES.saturating_sub(self.0.len()) {
            return Err(fmt::Error);
        }
        self.0.push_str(text);
        Ok(())
    }
}

fn crashes(value: &Value) -> Option<Vec<String>> {
    let items = value.items()?;
    if items.len() > MAX_CRASHES { return None; }
    items.iter().map(|item| {
        let mut text = CrashText(String::new());
        // BigLong Debug's temporary hex text is bounded by the inflated framing payload.
        write!(&mut text, "{item:?}").ok()?;
        if text.0.contains('\0') { None } else { Some(text.0) }
    }).collect()
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "eventLog" { return None; }
    match call.method {
        "LogClientStats" => Some(async {
            if let Some(value) = call.args.get(3) {
                let Some(items) = crashes(value) else { return Ok(Value::None); };
                for crash in items {
                    client_errors::add_crash(&c.db, c.login.user_id, c.session.int("charid"), &crash).await.map_err(db_err)?;
                }
            }
            Ok(Value::None)
        }.await),
        // clientStatsSvc.py:314
        "LogPlayerRequestedDisconnect" => Some(Ok(Value::None)),
        // eve/client/script/parklife/autopilot.py:79
        "LogClientEvent" => Some(Ok(Value::None)),
        _ => None,
    }
}

#[cfg(test)] mod tests;
