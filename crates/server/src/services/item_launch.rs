//! Shared decoding and per-entry error encoding for eveMisc.LaunchFromShip.
//! The client supplies inventory item IDs and quantities; inventory validation
//! and deployment authorization remain the responsibility of each launch service.
use std::collections::HashSet;
use marshal::Value;
use crate::services::CallError;

pub(crate) fn parse(args: &[Value]) -> Result<(Vec<(i64, i32)>, Option<i64>), ()> {
    let [Value::List(rows), behalf, warning] = args else { return Err(()) };
    let behalf = match behalf {
        Value::None => None,
        Value::Int(id) if *id > 0 => Some(*id),
        _ => return Err(()),
    };
    if !matches!(warning, Value::Bool(_) | Value::Int(0 | 1)) { return Err(()); }
    let mut seen = HashSet::new();
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Tuple(pair) = row else { return Err(()) };
        let [Value::Int(id), Value::Int(count)] = pair.as_slice() else { return Err(()) };
        let count = i32::try_from(*count).ok().filter(|v| *v > 0).ok_or(())?;
        if *id <= 0 || !seen.insert(*id) { return Err(()); }
        result.push((*id, count));
    }
    Ok((result, behalf))
}

pub(crate) fn error_value(error: CallError) -> Result<Value, CallError> {
    match error {
        CallError::User { msg, dict } => Ok(Value::list([Value::tuple([Value::str(msg), Value::Dict(dict.into_iter().map(|(k, v)| (Value::str(k), v)).collect())])])),
        error => Err(error),
    }
}

#[cfg(test)] mod tests;
