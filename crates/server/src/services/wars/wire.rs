//! Marshal representation of mutual wars for `evewar/util.py` and war services.
//! Reads durable `game.wars` records via the wars DAO and encodes field changes; no wire state is cached.
//! Aggressive wars, allies, bills and headquarters are not represented.

use crate::dao::wars::{State, War};
use marshal::Value;

/// `evewar/const.py`: player-declared war start reason.
const STARTED_PLAYER_DECLARED: i64 = 1;
/// `evewar/const.py`: mutual-war retraction end reason.
const ENDED_MUTUAL_END: i64 = 11;

const FIELD_NAMES: [&str; 17] = [
    "warID", "declaredByID", "againstID", "timeDeclared", "timeFinished", "retracted",
    "timeStarted", "retractedBy", "billID", "mutual", "allies", "createdFromWarID",
    "openForAllies", "canBeRetracted", "reasonEnded", "warHQ", "reasonStarted",
];

fn fields(row: &War) -> [Value; 17] {
    let optional = |value: Option<i64>| value.map(Value::Int).unwrap_or(Value::None);
    [
        Value::Int(row.war_id), Value::Int(row.declared_by), Value::Int(row.against),
        Value::Int(row.declared_at), optional(row.finishes_at), optional(row.retracted_at),
        Value::Int(row.starts_at), optional(row.retracted_by), Value::None, Value::Bool(true),
        Value::Dict(vec![]), Value::None, Value::Bool(false),
        Value::Bool(row.retracted_at.is_none() && row.state != State::Ended),
        optional(row.retracted_at.map(|_| ENDED_MUTUAL_END)), Value::None,
        Value::Int(STARTED_PLAYER_DECLARED),
    ]
}

pub(super) fn war(row: &War) -> Value {
    Value::reduce("warUtil.War", Vec::from(fields(row)), None)
}

pub(super) fn changes(old: Option<&War>, new: &War) -> Value {
    let old = old.map(fields);
    Value::Dict(FIELD_NAMES.into_iter().zip(fields(new)).enumerate().filter_map(|(i, (name, value))| {
        let previous = old.as_ref().map(|values| &values[i]).unwrap_or(&Value::None);
        (old.is_none() || previous != &value)
            .then(|| (Value::str(name), Value::tuple([previous.clone(), value])))
    }).collect())
}

pub(super) fn wars(rows: &[War]) -> Value {
    Value::Dict(rows.iter().map(|row| (Value::Int(row.war_id), war(row))).collect())
}

pub(super) fn war_list(rows: &[War]) -> Value {
    Value::list(rows.iter().map(war))
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
