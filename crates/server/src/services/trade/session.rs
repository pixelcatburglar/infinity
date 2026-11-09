//! Trade-session validation of the `pvptradewnd.py` offer manifest.
//! Root IDs are read from decoded `blue.DBRow` descriptors; malformed rows
//! cannot be silently discarded. Fitted items are outside direct-trade scope.
use std::collections::BTreeSet;
use marshal::{DbRowDescriptor, Value};
use infinity_core::consts::CLIENT_MAX_ISK;
use crate::{dao::trades::{Trade, TradeItem}, services::{market::round2, value_field}};

pub(super) fn money(value: &Value) -> Option<f64> {
    let amount = value.as_float()?;
    if !amount.is_finite() || !(0.0..=CLIENT_MAX_ISK).contains(&amount) { return None; }
    let rounded = round2(amount);
    rounded.is_finite().then_some(rounded)
}

pub(super) fn matches_manifest(value: &Value, trade: &Trade, roots: &[TradeItem]) -> bool {
    let Some(id) = value_field(value, "tradeContainerID").and_then(Value::as_int) else { return false };
    if id != trade.id { return false; }
    let Some([a, b]) = value_field(value, "money").and_then(Value::items) else { return false };
    if money(a) != Some(trade.money_a) || money(b) != Some(trade.money_b) { return false; }
    let Some(rows) = value_field(value, "tradeItems").and_then(Value::items) else { return false };
    let mut ids = BTreeSet::new();
    for row in rows {
        let Value::DbRow { descriptor, .. } = row else { return false };
        let Some(id) = DbRowDescriptor::from_value(descriptor).and_then(|desc| desc.get(row, "itemID")).and_then(|value| value.as_int()) else { return false };
        if !ids.insert(id) { return false; }
    }
    ids == roots.iter().map(|item| item.id).collect()
}
