//! Client exception telemetry for carbon/common/script/sys/alert.py.
//! Persists first traces, batched counts and detail reports in game.client_errors
//! and game.client_error_reports. First trace wins per hash; modified clients can
//! poison a hash because Python 2 non-ASCII stack bytes cannot be reproduced reliably.
//! Operators can delete a poisoned row after deleting its dependent reports. Flood
//! control is not modelled. Batches and groups allow 1024 records, counts up to
//! 1000000, and detail arguments up to 64 KiB; larger or malformed calls are rejected.
//! Detail log arguments escape control characters to keep one physical line per row.
use std::{borrow::Cow, collections::HashMap, io::Write};
use infinity_core::consts::FILETIME_UNIX_EPOCH;
use crate::{client::{Client, filetime_now, unix_seconds_from_filetime}, dao::{characters::selection, client_errors}, services::{Call, CallResult, db_err}};
use marshal::Value;
use tracing::{info, warn};

/// Server policy: retain the most useful 64 KiB of each stored traceback.
const TRACE_BYTES: usize = 64 * 1024;
/// Server policy: limit direct log trace records to a 1 MiB tail.
const LOG_TRACE_BYTES: usize = 1024 * 1024;
/// Server policy: bound work per batch and per decoded group.
const MAX_RECORDS: usize = 1024;
/// Server policy: bound the client-supplied repeat count per record.
const MAX_COUNT: i64 = 1_000_000;
/// Server policy: bound detail argument text without truncating it.
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
/// Client miniStack limit from stdlogutils.
const MAX_STACK_CHARS: usize = 4000;

fn tail(text: &str, limit: usize) -> &str {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) { start += 1; }
    &text[start..]
}
fn integer(v: &Value) -> Option<i64> { match v { Value::Int(i) => Some(*i), _ => None } }
fn text(v: &Value) -> Option<Cow<'_, str>> {
    let text = match v { Value::Bytes(b) => std::str::from_utf8(b).ok()?, Value::Unicode(s) => s, _ => return None };
    if text.contains('\0') { None } else { Some(Cow::Borrowed(text)) }
}
fn tuple(v: &Value) -> Option<&[Value]> { if let Value::Tuple(v) = v { Some(v) } else { None } }
fn seconds(t: i64) -> i64 { unix_seconds_from_filetime(t.clamp(FILETIME_UNIX_EPOCH, filetime_now())) }
fn count(v: &Value) -> Option<i64> { integer(v).filter(|n| (1..=MAX_COUNT).contains(n)) }
fn id(v: &Value) -> Option<i64> { integer(v).filter(|n| *n > 0) }
fn character(v: &Value) -> Option<Option<i64>> { match v { Value::None => Some(None), _ => id(v).map(Some) } }
fn log(mut writer: impl Write, record: String) {
    if let Err(e) = writer.write_all(record.as_bytes()) { warn!(error = %e, "client error log write failed"); }
}
fn batch(value: &Value) -> Option<Vec<(i64, i64, i64)>> {
    let Value::Dict(entries) = value else { return None; };
    if entries.len() > MAX_RECORDS { return None; }
    entries.iter().map(|(key, value)| {
        let [n, time] = tuple(value)? else { return None; };
        Some((id(key)?, count(n)?, integer(time)?))
    }).collect()
}
struct Detail<'a> { id: i64, user: i64, character: Option<i64>, count: i64, time: i64, args: Cow<'a, str> }
fn group(value: &Value) -> Option<Vec<Detail<'_>>> {
    let Value::Dict(entries) = value else { return None; };
    if entries.len() > MAX_RECORDS { return None; }
    let mut result = Vec::new();
    for (key, users) in entries {
        let error_id = id(key)?;
        let Value::Dict(users) = users else { return None; };
        if users.len() > MAX_RECORDS - result.len() { return None; }
        for (user, value) in users {
            let [n, time, ch, location1, location2, args, Value::None] = tuple(value)? else { return None; };
            // Locations are optional client IDs; their values are not persisted.
            for location in [location1, location2] { if !matches!(location, Value::None) && integer(location).is_none() { return None; } }
            let args = text(args)?;
            if args.len() > MAX_ARGUMENT_BYTES { return None; }
            result.push(Detail { id: error_id, user: id(user)?, character: character(ch)?, count: count(n)?, time: integer(time)?, args });
        }
    }
    Some(result)
}
fn stack(value: &Value) -> Option<(i64, Cow<'_, str>)> {
    let [hash, key] = tuple(value)? else { return None; };
    let key = text(key)?;
    if key.chars().count() > MAX_STACK_CHARS { return None; }
    Some((integer(hash)?, key))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "alert" { return None; }
    Some(match call.method {
        "BeanCount" => async {
            let Some(hash) = call.args.first().and_then(integer) else { return Ok(Value::tuple([Value::None, Value::None])); };
            let found = client_errors::lookup(&c.db, hash).await.map_err(db_err)?;
            Ok(match found { Some(row) => Value::tuple([Value::Int(row.error_id), Value::str(row.log_mode)]), None => Value::tuple([Value::None, Value::None]) })
        }.await,
        "SendClientStackTraceAlert" => async {
            let [stack_value, trace, mode, next] = call.args else { return Ok(Value::None); };
            let (Some((hash, key)), Some(trace), Some(mode)) = (stack(stack_value), text(trace), text(mode)) else { return Ok(Value::None); };
            if !matches!(mode.as_ref(), "Error" | "Warning" | "Info") || (!matches!(next, Value::None) && integer(next).is_none()) { return Ok(Value::None); }
            let ch = c.session.int("charid");
            let row = client_errors::upsert(&c.db, hash, &key, &mode, &c.cfg.telemetry.client_error_log_mode, c.login.user_id, ch, tail(&trace, TRACE_BYTES)).await.map_err(db_err)?;
            if row.inserted { info!(error_id = row.error_id, hash, first_line = trace.lines().next().unwrap_or(""), "new client error"); }
            if let Some(writer) = crate::logging::client_errors() {
                let log_trace = tail(&trace, LOG_TRACE_BYTES);
                let truncated = if log_trace.len() < trace.len() { " truncated=true" } else { "" };
                log(writer, format!("==== {} user={} char={} error_id={} hash={} mode={}{}\n{}\n\n", row.timestamp, c.login.user_id, ch.map(|v| v.to_string()).unwrap_or_else(|| "None".into()), row.error_id, hash, mode, truncated, log_trace));
            }
            Ok(Value::tuple([Value::Int(row.error_id), Value::str(row.log_mode)]))
        }.await,
        "BeanDelivery" => async {
            let Some(records) = call.args.first().and_then(batch) else { return Ok(Value::None); };
            for (id, count, time) in records {
                client_errors::add_count(&c.db, id, count, seconds(time)).await.map_err(db_err)?;
            }
            Ok(Value::None)
        }.await,
        "GroupBeanDelivery" => async {
            let decoded;
            let data = match call.args.first() {
                Some(Value::Bytes(bytes)) => { decoded = crate::framing::inflate(bytes).ok().and_then(|b| marshal::loads(&b).ok()); decoded.as_ref() }
                v => v,
            };
            let Some(records) = data.and_then(group) else { return Ok(Value::None); };
            let mut owners = HashMap::new();
            for record in &records {
                if record.user != c.login.user_id { warn!(user = record.user, "rejecting spoofed client error report"); continue; }
                if let Some(ch) = record.character {
                    if let std::collections::hash_map::Entry::Vacant(entry) = owners.entry(ch) {
                        entry.insert(selection::owns(&c.db, ch, c.login.user_id).await.map_err(db_err)?);
                    }
                }
            }
            for record in records {
                if record.user != c.login.user_id { continue; }
                if record.character.is_some_and(|ch| !owners[&ch]) { warn!(character = ?record.character, "rejecting foreign character client error report"); continue; }
                let timestamp = client_errors::add_report(&c.db, record.id, record.user, record.character, record.count, seconds(record.time), &record.args).await.map_err(db_err)?;
                if let Some(writer) = crate::logging::client_errors() {
                    log(writer, format!("---- {timestamp} user={} char={} error_id={} count={} args={}\n", record.user, record.character.map(|v| v.to_string()).unwrap_or_else(|| "None".into()), record.id, record.count, record.args.escape_debug()));
                }
            }
            Ok(Value::None)
        }.await,
        "GetLogModeForError" => async {
            let mut changes = Vec::new();
            if let Some(Value::Dict(entries)) = call.args.first() {
                for (hash_value, current) in entries {
                    let (Some(hash), Some([id_value, mode])) = (integer(hash_value), tuple(current)) else { continue; };
                    let (Some(_), Some(mode)) = (id(id_value), text(mode)) else { continue; };
                    if let Some(row) = client_errors::lookup(&c.db, hash).await.map_err(db_err)? {
                        if row.log_mode.as_str() != mode.as_ref() { changes.push((hash_value.clone(), Value::str(row.log_mode))); }
                    }
                }
            }
            Ok(Value::tuple([Value::Dict(changes), Value::list([])]))
        }.await,
        _ => return None,
    })
}

#[cfg(test)] mod tests;
