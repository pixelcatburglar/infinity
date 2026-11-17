//! Dogma effect environment wire values, consumed by the client dogma environment.
//! Values come from the active module and its target and charge; no state is stored here.

use marshal::Value;

/// `dogma.effects.environment.Environment.Line`: item, character, ship, target,
/// other, area, effect and structure. A charge's other ID may be a sublocation tuple.
pub(crate) fn build(item: i64, char_id: i64, ship: i64, target: Option<i64>, other: Option<Value>, effect: i64) -> Value {
    Value::list([
        Value::Int(item),
        Value::Int(char_id),
        Value::Int(ship),
        target.map(Value::Int).unwrap_or(Value::None),
        other.unwrap_or(Value::None),
        Value::list([]),
        Value::Int(effect),
        Value::None,
    ])
}

#[cfg(all(test, feature = "tests-effects"))] mod tests;
