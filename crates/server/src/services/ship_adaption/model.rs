//! Strict authenticated `shipAdaptionMgr` request decoding before Lua receives it.
//! The client sends Python integer arguments and old-style KeyVal exchange rows;
//! bound calls, keyword arguments and all other object shapes are unsupported.

use marshal::Value;
use crate::services::Call;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Tree(u32), Missions, User,
    Inject { item: i64, quantity: u32 },
    Exchange { issuer: i64, chips: Vec<(i32, u32)> },
}

fn positive_u32(value: &Value) -> Option<u32> {
    let Value::Int(value) = value else { return None };
    u32::try_from(*value).ok().filter(|value| *value > 0)
}

fn entry(value: &Value) -> Option<(i32, u32)> {
    let Value::Instance(class, state) = value else { return None };
    if !matches!(class.as_ref(), Value::Bytes(bytes) if bytes == b"utillib.KeyVal") { return None; }
    let Value::Dict(fields) = state.as_ref() else { return None };
    if fields.len() != 2 { return None; }
    let mut type_id = None;
    let mut quantity = None;
    for (key, value) in fields {
        if matches!(key, Value::Bytes(bytes) if bytes == b"type_id") && type_id.is_none() {
            type_id = positive_u32(value).and_then(|id| i32::try_from(id).ok());
            if type_id.is_none() { return None; }
        } else if matches!(key, Value::Bytes(bytes) if bytes == b"quantity") && quantity.is_none() {
            quantity = positive_u32(value);
            if quantity.is_none() { return None; }
        } else { return None; }
    }
    Some((type_id?, quantity?))
}

/// `catalogue_len` bounds exchange allocation independently of supplied input.
pub(crate) fn decode(call: &Call<'_>, user: i64, character: i64, catalogue_len: usize) -> Option<Request> {
    if call.service != "shipAdaptionMgr" || call.bound || call.bind_params.is_some()
        || !(matches!(call.kwargs, Value::None) || matches!(call.kwargs, Value::Dict(v) if v.is_empty()))
        || i32::try_from(user).ok().filter(|id| *id > 0).is_none()
        || u32::try_from(character).ok().filter(|id| *id > 0).is_none() { return None; }
    match (call.method, call.args) {
        ("fetch_tree_data", [tree]) => Some(Request::Tree(positive_u32(tree)?)),
        ("fetch_missions", []) => Some(Request::Missions),
        ("fetch_user_data", []) => Some(Request::User),
        ("InjectChips", [Value::Int(item), quantity]) if *item > 0 => {
            let quantity = positive_u32(quantity)?;
            Some(Request::Inject { item: *item, quantity })
        }
        ("call_chip_exchange", [issuer, Value::List(rows)]) if !rows.is_empty() && rows.len() <= catalogue_len => {
            let issuer = i64::from(positive_u32(issuer)?);
            let mut chips = Vec::with_capacity(rows.len());
            for row in rows { chips.push(entry(row)?); }
            chips.sort_unstable_by_key(|entry| entry.0);
            if chips.windows(2).any(|pair| pair[0].0 == pair[1].0) { return None; }
            Some(Request::Exchange { issuer, chips })
        }
        _ => None,
    }
}
