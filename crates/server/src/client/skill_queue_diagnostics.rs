//! Privacy-preserving diagnostics for server skill-queue RPCs and notifications.
//! Values are observed only at the existing client boundaries; no I/O or game work occurs.

use super::Client;
use crate::hash::sha256_hex_chunks;
use infinity_wire::macho::{self, Address, Packet};
use crate::services::{Call, CallError, CallResult};
use infinity_core::consts::dogma::CHAR_ATTRIBUTES;
use marshal::Value;
use std::fmt::Write;
use tracing::Level;

/// Server diagnostic policy: tracing target for skill queue observations.
const TARGET: &str = "infinity_server::client::skill_queue";
/// Server diagnostic format: fixed queue field order.
const QUEUE_FIELDS: [&str; 7] = [
    "queuePosition",
    "trainingTypeID",
    "trainingToLevel",
    "trainingStartSP",
    "trainingDestinationSP",
    "trainingStartTime",
    "trainingEndTime",
];
/// Server diagnostic format: queue projection domain and nine-byte records.
const QUEUE_DOMAIN: [u8; 9] = *b"sqdiag-q1";
/// Server diagnostic format: distinct domains for ordered request projections.
const SAVE_DOMAIN: [u8; 9] = *b"sqdiag-s1";
/// Server diagnostic format: ordered type ID projection domain.
const TYPES_DOMAIN: [u8; 9] = *b"sqdiag-t1";
/// Server diagnostic format: ordered skill target projection domain.
const TARGET_DOMAIN: [u8; 9] = *b"sqdiag-p1";
/// Server diagnostic format: ordered allocation projection domain.
const ALLOCATION_DOMAIN: [u8; 9] = *b"sqdiag-a1";
/// Server diagnostic format: count, integer, None, missing and unexpected tags.
const COUNT_TAG: u8 = b'c';
/// Server diagnostic format: integer scalar tag.
const INT_TAG: u8 = b'i';
/// Server diagnostic format: None scalar tag.
const NONE_TAG: u8 = b'n';
/// Server diagnostic format: missing scalar tag.
const MISSING_TAG: u8 = b'm';
/// Server diagnostic format: unexpected scalar tag.
const UNEXPECTED_TAG: u8 = b'u';

/// Server diagnostic format: a typed queue scalar, including invalid shapes.
#[derive(Clone, Copy)]
enum QueueScalar {
    Int(i64),
    None,
    Missing,
    Unexpected(&'static str),
}

impl QueueScalar {
    fn record(self) -> [u8; 9] {
        let mut record = [0; 9];
        match self {
            Self::Int(value) => {
                record[0] = INT_TAG;
                record[1..].copy_from_slice(&value.to_le_bytes());
            }
            Self::None => record[0] = NONE_TAG,
            Self::Missing => record[0] = MISSING_TAG,
            Self::Unexpected(_) => record[0] = UNEXPECTED_TAG,
        }
        record
    }
}

#[derive(Clone, Copy)]
pub(super) enum RpcWrite { Ok, Failed, NotAttempted }

pub(super) struct RpcDiagnostic {
    character_id: Option<i64>, session_id: i64, client_id: i64, call_id: Option<i64>,
    service: &'static str, method: String, outer_method: Option<&'static str>, bound: bool, dispatch_start_ft: i64,
    request: String, trace: bool, result_ready_ft: Option<i64>, result: String, outcome: &'static str,
}

fn selected(service: &str, method: &str) -> bool {
    (service == "alert" && method == "BeanCount") || (service == "skillHandler" && matches!(method,
        "SaveNewQueue" | "GetSkillQueueAndFreePoints" | "GetSkillQueue" | "AbortTraining" |
        "PurchaseSkills" | "GetSkillChangesForISIS" | "GetFreeSkillPointsAppliedToSkills" |
        "ApplyFreeSkillPointsToSkills" | "GetFreeSkillPointsAppliedToQueue" |
        "ApplyFreeSkillPointsToQueue" | "ApplyFreeSkillPoints" | "GetFreeSkillPoints" |
        "GetAttributes" | "GetSkills" | "GetAllSkills" | "GetBoosters" | "GetImplants"))
}
fn kind(v: &Value) -> &'static str {
    match v {
        Value::None => "none",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bytes(_) => "bytes",
        Value::Unicode(_) => "unicode",
        Value::Tuple(_) => "tuple",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Instance(_, _) => "instance",
        _ => "other",
    }
}
fn strict_int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}
fn activation_value(v: &Value) -> String { match v { Value::Int(n) => n.to_string(), Value::Bool(b) => b.to_string(), Value::None => "None".into(), _ => format!("unexpected({})", kind(v)) } }
fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> { crate::services::value_field(v, name) }
fn escape(s: &str) -> String { s.chars().fold(String::with_capacity(s.len()), |mut o, c| { match c { '\\' => o.push_str("\\\\"), '\n' => o.push_str("\\n"), '\r' => o.push_str("\\r"), '\t' => o.push_str("\\t"), c if c.is_control() => o.push_str(&format!("\\u{:04x}", c as u32)), c => o.push(c) }; o }) }

fn queue_row(v: &Value) -> [QueueScalar; 7] {
    std::array::from_fn(|i| match field(v, QUEUE_FIELDS[i]) {
        Some(Value::Int(n)) => QueueScalar::Int(*n),
        Some(Value::None) if i >= 5 => QueueScalar::None,
        Some(value) => QueueScalar::Unexpected(kind(value)),
        None => QueueScalar::Missing,
    })
}
fn queue_projection(queue: &[Value], trace: bool) -> (usize, String, String) {
    let mut count = [0u8; 9];
    count[0] = COUNT_TAG;
    count[1..].copy_from_slice(&(queue.len() as u64).to_le_bytes());

    let mut bad = false;
    let mut detail = String::new();
    if trace {
        detail.push_str(" items=[");
    }
    let rows = queue.iter().enumerate().flat_map(|(index, value)| {
        let row = queue_row(value);
        if trace {
            if index != 0 {
                detail.push(',');
            }
            detail.push('[');
        }
        for (field_index, scalar) in row.iter().enumerate() {
            if trace && field_index != 0 {
                detail.push(',');
            }
            match scalar {
                QueueScalar::Int(n) => {
                    if trace {
                        let _ = write!(detail, "{n}");
                    }
                }
                QueueScalar::None => {
                    if trace {
                        detail.push_str("None");
                    }
                }
                QueueScalar::Missing => {
                    bad = true;
                    if trace {
                        detail.push_str("missing");
                    }
                }
                QueueScalar::Unexpected(value_kind) => {
                    bad = true;
                    if trace {
                        let _ = write!(detail, "unexpected({value_kind})");
                    }
                }
            }
        }
        if trace {
            detail.push(']');
        }
        row.into_iter().map(QueueScalar::record)
    });
    let digest = sha256_hex_chunks(
        std::iter::once(QUEUE_DOMAIN)
            .chain(std::iter::once(count))
            .chain(rows),
    );
    if trace {
        detail.push(']');
    }
    (queue.len(), digest, format!("shape={}{detail}", if bad { "unexpected" } else { "ok" }))
}
fn integer(v: Option<&Value>) -> QueueScalar {
    match v {
        Some(Value::Int(n)) => QueueScalar::Int(*n),
        Some(v) => QueueScalar::Unexpected(kind(v)),
        None => QueueScalar::Missing,
    }
}
fn integer_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::None) => "None".into(),
        Some(v) => format!("unexpected({})", kind(v)),
        None => "missing".into(),
    }
}
fn pair(v: &Value) -> [QueueScalar; 2] {
    match v {
        Value::List(p) | Value::Tuple(p) if p.len() == 2 => [integer(p.first()), integer(p.get(1))],
        _ => [QueueScalar::Unexpected(kind(v)), QueueScalar::Missing],
    }
}
fn projection<const N: usize>(
    domain: [u8; 9], count: usize, rows: impl Iterator<Item = [QueueScalar; N]>, trace: bool,
) -> String {
    let mut count_record = [0; 9];
    count_record[0] = COUNT_TAG;
    count_record[1..].copy_from_slice(&(count as u64).to_le_bytes());
    let mut bad = false;
    let mut detail = String::new();
    if trace { detail.push_str(" values=["); }
    let records = rows.enumerate().flat_map(|(index, row)| {
        if trace {
            if index != 0 { detail.push(','); }
            detail.push('(');
        }
        for (i, value) in row.iter().enumerate() {
            if trace && i != 0 { detail.push(','); }
            match value {
                QueueScalar::Int(n) => { if trace { let _ = write!(detail, "{n}"); } }
                QueueScalar::None => { if trace { detail.push_str("None"); } }
                QueueScalar::Missing => { bad = true; if trace { detail.push_str("missing"); } }
                QueueScalar::Unexpected(k) => { bad = true; if trace { let _ = write!(detail, "unexpected({k})"); } }
            }
        }
        if trace { detail.push(')'); }
        row.into_iter().map(QueueScalar::record)
    });
    let digest = sha256_hex_chunks(std::iter::once(domain).chain(std::iter::once(count_record)).chain(records));
    if trace { detail.push(']'); }
    format!("count={count} sha256={digest} shape={}{detail}", if bad { "unexpected" } else { "ok" })
}
fn invalid_shape(v: Option<&Value>) -> String {
    match v {
        None => "shape=missing".into(),
        Some(v) => format!("shape=unexpected({})", kind(v)),
    }
}
fn type_list(v: Option<&Value>, trace: bool) -> String {
    match v {
        Some(Value::List(xs)) => projection(TYPES_DOMAIN, xs.len(), xs.iter().map(|v| [integer(Some(v))]), trace),
        _ => invalid_shape(v),
    }
}
fn targets(v: Option<&Value>, trace: bool) -> String {
    match v {
        Some(Value::List(xs) | Value::Tuple(xs)) => projection(TARGET_DOMAIN, xs.len(), xs.iter().map(pair), trace),
        _ => invalid_shape(v),
    }
}
fn save_input(v: Option<&Value>, trace: bool) -> String {
    match v {
        Some(Value::Dict(xs)) => projection(SAVE_DOMAIN, xs.len(), xs.iter().map(|(position, value)| {
            let [type_id, level] = pair(value);
            [integer(Some(position)), type_id, level]
        }), trace),
        _ => invalid_shape(v),
    }
}
fn allocations(v: &Value, trace: bool) -> String {
    match v {
        Value::Dict(xs) => projection(ALLOCATION_DOMAIN, xs.len(), xs.iter().map(|(k, v)| [integer(Some(k)), integer(Some(v))]), trace),
        _ => invalid_shape(Some(v)),
    }
}
fn queue_result(v: Option<&Value>, trace: bool) -> String {
    match v {
        Some(Value::List(xs)) => {
            let (n, d, shape) = queue_projection(xs, trace);
            format!("entry_count={n} queue_sha256={d} {shape}")
        }
        _ => invalid_shape(v),
    }
}
fn collection(v: &Value, label: &str) -> String {
    let count = match v { Value::List(xs) | Value::Tuple(xs) => Some(xs.len()), Value::Dict(xs) => Some(xs.len()), _ => None };
    match count {
        Some(n) => format!("{label}={n} response_shape={}", kind(v)),
        None => format!("response_shape=unexpected({})", kind(v)),
    }
}

fn request(call: &Call<'_>, trace: bool) -> String { match (call.service, call.method) {
    ("skillHandler", "SaveNewQueue") => { let a = call.kwargs.get("activate").or_else(|| call.args.get(1)); let source = if call.kwargs.get("activate").is_some() { "keyword" } else if call.args.get(1).is_some() { "positional" } else { "default" }; let value = a.map_or("kind=bool value=true".into(), |v| format!("kind={} value={}", kind(v), activation_value(v))); format!("input={} activate_source={source} activate={value}", save_input(call.args.first(), trace)) }
    ("skillHandler", "PurchaseSkills") => format!("type_ids={}", type_list(call.args.first(), trace)),
    ("skillHandler", "GetFreeSkillPointsAppliedToSkills" | "ApplyFreeSkillPointsToSkills") => format!("targets={}", targets(call.args.first(), trace)),
    ("skillHandler", "ApplyFreeSkillPoints") => format!("type_id={} points={}", integer_text(call.args.first()), integer_text(call.args.get(1))),
    ("alert", "BeanCount") => format!("stack_hash={}", match call.args.first() { Some(Value::Int(n)) => n.to_string(), v => invalid_shape(v) }),
    _ => format!("args={} kwargs_shape={}", call.args.len(), kind(call.kwargs)),
} }

pub(super) fn begin(c: &Client, packet: &Packet, call: &Call<'_>) -> Option<RpcDiagnostic> {
    if !tracing::enabled!(target: TARGET, Level::DEBUG) { return None; }
    let inner = if call.service == "skillHandler" && call.method == "MachoBindObject" {
        call.args.get(1).and_then(Value::items).and_then(|parts| match parts {
            [method, args, kwargs] => method.as_text().filter(|method| selected(call.service, method)).map(|method| (method, args, kwargs)),
            _ => None,
        })
    } else { None };
    if inner.is_none() && !selected(call.service, call.method) { return None; }
    let call_id = match &packet.source { Address::Client { call_id, .. } | Address::Node { call_id, .. } | Address::Any { call_id, .. } => strict_int(Some(call_id)), Address::Broadcast { .. } => None };
    let trace = tracing::enabled!(target: TARGET, Level::TRACE);
    let dispatch_start_ft = super::filetime_now();
    let (method, bound, outer_method, request) = match inner {
        Some((method, args, kwargs)) => {
            let inner_call = Call { service: call.service, method: &method, args: args.items().unwrap_or(&[]), kwargs, bound: true, bind_params: None };
            let request = request(&inner_call, trace);
            (method.into_owned(), true, Some("MachoBindObject"), request)
        }
        None => (call.method.into(), call.bound, None, request(call, trace)),
    };
    Some(RpcDiagnostic { character_id: c.session.int("charid").filter(|n| *n != 0), session_id: c.session.sid, client_id: c.login.client_id, call_id, service: if call.service == "alert" { "alert" } else { "skillHandler" }, method, outer_method, bound, dispatch_start_ft, request, trace, result_ready_ft: None, result: String::new(), outcome: "ok" })
}

impl RpcDiagnostic {
    pub(super) fn observe_result(&mut self, result: &CallResult) {
        self.result_ready_ft = Some(super::filetime_now());
        match result {
            Err(CallError::User { .. }) => { self.outcome = "user_error"; self.result = "error=user".into(); }
            Err(CallError::Internal(_)) => { self.outcome = "internal_error"; self.result = "error=internal".into(); }
            Ok(value) => {
                let value = if self.outer_method.is_some() {
                    match value {
                        Value::Tuple(parts) if parts.len() == 2 => &parts[1],
                        _ => { self.result = "shape=unexpected".into(); self.outcome = "ok"; return; }
                    }
                } else { value };
                self.result = match (self.service, self.method.as_str()) {
                ("skillHandler", "GetSkillQueue") => format!("{} free_points=missing", queue_result(Some(value), self.trace)),
                ("skillHandler", "GetSkillQueueAndFreePoints") => match value {
                    Value::Tuple(p) if p.len() == 2 => format!("{} free_points={}", queue_result(p.first(), self.trace), integer_text(p.get(1))),
                    _ => invalid_shape(Some(value)),
                },
                ("skillHandler", "GetFreeSkillPoints" | "ApplyFreeSkillPoints") => format!("free_points={}", integer_text(Some(value))),
                ("skillHandler", "SaveNewQueue" | "AbortTraining" | "ApplyFreeSkillPointsToSkills" | "ApplyFreeSkillPointsToQueue") => match value {
                    Value::None => "result=None".into(),
                    _ => format!("result=unexpected({})", kind(value)),
                },
                ("skillHandler", "PurchaseSkills") => format!("type_ids={}", type_list(Some(value), self.trace)),
                ("skillHandler", "GetFreeSkillPointsAppliedToSkills" | "GetFreeSkillPointsAppliedToQueue") => format!("allocations={}", allocations(value, self.trace)),
                ("skillHandler", "GetAttributes") => {
                    let entries = match value { Value::Dict(p) => Some(p), _ => None };
                    let mut text = String::new();
                    for id in CHAR_ATTRIBUTES {
                        if !text.is_empty() { text.push(','); }
                        let v = entries.and_then(|p| p.iter().find(|(k, _)| strict_int(Some(k)) == Some(id)).map(|(_, v)| v));
                        let projected = match v {
                            Some(Value::Int(n)) => n.to_string(),
                            Some(Value::Float(n)) => n.to_string(),
                            Some(v) => format!("unexpected({})", kind(v)),
                            None if entries.is_none() => format!("unexpected({})", kind(value)),
                            None => "missing".into(),
                        };
                        let _ = write!(text, "{id}={projected}");
                    }
                    format!("attributes={text}")
                }
                ("alert", "BeanCount") => {
                    let p = match value { Value::Tuple(p) => Some(p), _ => None };
                    let mode = match p.and_then(|p| p.get(1)) { Some(Value::Bytes(b)) if b == b"B" => "B", Some(Value::Bytes(b)) if b == b"D" => "D", Some(Value::None) => "None", Some(_) => "unexpected", None if p.is_some() => "missing", None => "unexpected" };
                    let error_id = p.map_or_else(|| "unexpected".into(), |p| integer_text(p.first()));
                    let shape = if p.is_some_and(|p| p.len() == 2) { "ok" } else { "unexpected" };
                    format!("error_id={error_id} log_mode={mode} shape={shape}")
                }
                ("skillHandler", "GetSkills" | "GetAllSkills") => collection(value, "skill_count"),
                ("skillHandler", "GetBoosters" | "GetImplants") => collection(value, "item_count"),
                _ => collection(value, "count"),
            }; self.outcome = "ok"; }
        }
    }
    pub(super) fn finish(self, response: Option<&Packet>, write: RpcWrite) {
        let (rw, completion, end) = match write { RpcWrite::Ok => ("ok", "reply_write_finished", Some(super::filetime_now())), RpcWrite::Failed => ("failed", "reply_write_finished", Some(super::filetime_now())), RpcWrite::NotAttempted => ("not_attempted", "pre_reply_flush_failed", None) };
        let request = escape(&self.request); let mut result = self.result;
        if let Some(packet) = response { if let Some(bytes) = packet.body.iter().find_map(Value::as_bytes) { result.push_str(&format!(" response_wire_sha256={}", sha256_hex_chunks([bytes]))); } }
        let result = escape(&result);
        if self.trace { tracing::trace!(target: TARGET, kind="rpc", character_id=?self.character_id, session_id=self.session_id, client_id=self.client_id, call_id=?self.call_id, service=self.service, method=self.method, outer_method=?self.outer_method, bound=self.bound, dispatch_start_ft=self.dispatch_start_ft, result_ready_ft=?self.result_ready_ft, write_end_ft=?end, outcome=self.outcome, reply_write=rw, completion, request, response=result, "skill queue rpc"); } else { tracing::debug!(target: TARGET, kind="rpc", character_id=?self.character_id, session_id=self.session_id, client_id=self.client_id, call_id=?self.call_id, service=self.service, method=self.method, outer_method=?self.outer_method, bound=self.bound, dispatch_start_ft=self.dispatch_start_ft, result_ready_ft=?self.result_ready_ft, write_end_ft=?end, outcome=self.outcome, reply_write=rw, completion, request, response=result, "skill queue rpc"); }
    }
}

fn notification_name(packet: &Packet) -> Option<&'static str> { if packet.command != macho::cmd::NOTIFICATION { return None; } match &packet.destination { Address::Broadcast { broadcast_id, .. } => match broadcast_id.as_text()?.as_ref() { "OnNewSkillQueueSaved" => Some("OnNewSkillQueueSaved"), "OnServerSkillsChanged" => Some("OnServerSkillsChanged"), "OnServerSkillsRemoved" => Some("OnServerSkillsRemoved"), "OnFreeSkillPointsChanged" => Some("OnFreeSkillPointsChanged"), "OnServerBoostersChanged" => Some("OnServerBoostersChanged"), "OnServerImplantsChanged" => Some("OnServerImplantsChanged"), "OnRespecInfoChanged" => Some("OnRespecInfoChanged"), "OnExpertSystemsUpdated" => Some("OnExpertSystemsUpdated"), _ => None }, _ => None } }
fn notification_detail(name: &str, args: &[Value], bytes: &[u8], trace: bool) -> String {
    match name {
        "OnNewSkillQueueSaved" => queue_result(args.first(), trace),
        "OnServerSkillsChanged" | "OnServerSkillsRemoved" | "OnExpertSystemsUpdated" => {
            let count = match args.first() {
                Some(Value::Dict(entries)) => format!("entry_count={} shape=ok", entries.len()),
                value => invalid_shape(value),
            };
            let timestamp = match name {
                "OnServerSkillsChanged" => format!(" timestamp={}", integer_text(args.get(2))),
                "OnServerSkillsRemoved" => format!(" timestamp={}", integer_text(args.get(1))),
                _ => String::new(),
            };
            format!("{count}{timestamp} payload_wire_sha256={}", sha256_hex_chunks([bytes]))
        }
        "OnFreeSkillPointsChanged" => {
            let shape = if args.len() == 1 && matches!(args.first(), Some(Value::Int(_))) { "ok" } else { "unexpected" };
            format!("arg_count={} shape={shape} free_points={}", args.len(), integer_text(args.first()))
        }
        _ => format!("arg_count={} shape={}", args.len(), if args.is_empty() { "ok" } else { "unexpected" }),
    }
}

pub(super) fn notification_write(c: &Client, packet: &Packet, ok: bool) {
    if !tracing::enabled!(target: TARGET, Level::DEBUG) { return; }
    let Some(name) = notification_name(packet) else { return; };
    let write_end_ft = super::filetime_now();
    let trace = tracing::enabled!(target: TARGET, Level::TRACE);
    let detail = match packet.body.as_slice() {
        [Value::Tuple(outer)] => match outer.as_slice() {
            [Value::Int(0), Value::Bytes(bytes)] => match marshal::loads(bytes) {
                Ok(Value::Tuple(inner)) => match inner.as_slice() {
                    [Value::Int(1), Value::Tuple(args)] => notification_detail(name, args, bytes, trace),
                    _ => "shape=unexpected".into(),
                },
                Ok(_) => "shape=unexpected".into(),
                Err(_) => "shape=decode_failed".into(),
            },
            _ => "shape=unexpected".into(),
        },
        _ => "shape=unexpected".into(),
    };
    let character_id = c.session.int("charid").filter(|n| *n != 0);
    let send = if ok { "ok" } else { "failed" };
    if trace { tracing::trace!(target: TARGET, kind="notification", character_id=?character_id, session_id=c.session.sid, client_id=c.login.client_id, name, send, write_end_ft, detail, "skill queue notification"); } else { tracing::debug!(target: TARGET, kind="notification", character_id=?character_id, session_id=c.session.sid, client_id=c.login.client_id, name, send, write_end_ft, detail, "skill queue notification"); }
}
