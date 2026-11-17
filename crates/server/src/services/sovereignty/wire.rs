//! Sovereignty projections for `sovereignty.data_types` and `sovSvc`.
//! Values come from committed Hub, system, policy and history rows plus the
//! projected development view; campaigns are not modelled here.
use marshal::{DbRowDescriptor, DbType, Value};
use crate::{dao::sovereignty::{history::HistoryRow, policy::PolicyRow, structures::HubRow, systems::SystemRow}, services::{crowset, db_err, keyval, rowset, CallError}};
use super::indices::model::DevelopmentView;

fn optional(id: Option<i64>) -> Value { id.map_or(Value::None, Value::Int) }

pub(crate) fn system_claim(row: &SystemRow) -> Value {
    match (row.hub_item_id, row.claim_corporation_id, row.alliance_id) {
        (Some(hub), Some(corporation), Some(alliance)) => Value::reduce("sovereignty.data_types.SovClaimInfo", vec![Value::Int(hub), Value::Int(corporation), Value::Int(alliance)], None),
        _ => Value::None,
    }
}

pub(crate) fn hub(row: Option<&SystemRow>) -> Value {
    match row.and_then(|r| Some((r.hub_item_id?, r.claim_corporation_id?, r.alliance_id?, r.claim_time?))) {
        Some((hub, corporation, alliance, time)) => Value::reduce("sovereignty.data_types.SovHubInfo", vec![Value::Int(hub), Value::Int(corporation), Value::Int(alliance), Value::Int(time)], None),
        None => Value::None,
    }
}

/// Complete physical structure list, including unclaimed neutral Hubs.
/// `development` is `Some` only while the whole index capability serves; every
/// Hub then requires exactly one valid view. `None` omits `defenseMultiplier`,
/// never a fabricated value.
pub(crate) fn structures(hubs: &[HubRow], policies: &[PolicyRow], development: Option<&[DevelopmentView]>) -> Result<Vec<Value>, CallError> {
    hubs.iter().map(|h| {
        let capital = h.system.alliance_id.is_some_and(|alliance| policies.iter().any(|p| p.alliance_id == alliance && p.current_capital_system == Some(h.system.system_id)));
        let mut fields = vec![
            ("itemID", Value::Int(h.structure.item_id)), ("typeID", Value::Int(i64::from(h.type_id))),
            ("solarSystemID", Value::Int(h.system.system_id)), ("corporationID", Value::Int(h.owner_id)),
            ("ownerID", Value::Int(h.owner_id)), ("allianceID", optional(h.system.alliance_id)),
            ("isCapital", Value::Bool(capital)), ("campaignState", Value::None), ("vulnerabilityState", Value::None),
        ];
        if let Some(views) = development {
            let mut matching = views.iter().filter(|view| view.system_id == h.system.system_id);
            let (Some(view), None) = (matching.next(), matching.next()) else {
                return Err(db_err(anyhow::anyhow!("development projection does not cover sovereignty Hub")));
            };
            if ![view.military_points, view.industrial_points, view.strategic_seconds, view.military.remainder, view.industrial.remainder, view.strategic.remainder, view.defense_multiplier].iter().all(|value| value.is_finite()) || view.military_points < 0.0 || view.industrial_points < 0.0 || view.strategic_seconds < 0.0 || view.defense_multiplier <= 0.0 {
                return Err(db_err(anyhow::anyhow!("invalid development defense multiplier")));
            }
            fields.push(("defenseMultiplier", Value::Float(view.defense_multiplier)));
        }
        Ok(keyval(fields))
    }).collect()
}

pub(crate) fn alliance_structures(hubs: &[HubRow]) -> Value {
    let header = DbRowDescriptor::new([
        ("structureID", DbType::I8), ("solarSystemID", DbType::I8), ("corporationID", DbType::I8),
        ("campaignStartTime", DbType::FileTime), ("campaignEventType", DbType::I4),
        ("campaignOccupancyLevel", DbType::R8), ("vulnerableStartTime", DbType::FileTime),
        ("vulnerableEndTime", DbType::FileTime), ("vulnerabilityOccupancyLevel", DbType::R8),
    ]);
    let rows: Vec<Value> = hubs.iter().map(|h| header.row(&[Value::Int(h.structure.item_id), Value::Int(h.system.system_id), Value::Int(h.owner_id), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None])).collect();
    let scores = DbRowDescriptor::new([("sourceItemID", DbType::I8), ("teamID", DbType::I8), ("score", DbType::R8)]);
    Value::tuple([crowset(&header, rows.clone()), crowset(&header, rows), crowset(&scores, vec![])])
}

pub(crate) fn capital(policy: Option<&PolicyRow>) -> Value {
    keyval([("currentCapitalSystem", optional(policy.and_then(|p| p.current_capital_system))), ("newCapitalSystem", Value::None), ("newCapitalSystemValidAfter", Value::None)])
}

pub(crate) fn alliance_systems(rows: &[SystemRow]) -> Value {
    rowset(&["solarSystemID", "allianceID"], rows.iter().filter_map(|r| Some(vec![Value::Int(r.system_id), Value::Int(r.alliance_id?)] )).collect())
}

pub(crate) fn current_map(ids: &[i64], rows: &[SystemRow]) -> Value {
    let by_system: std::collections::HashMap<_, _> = rows.iter().map(|r| (r.system_id, r)).collect();
    rowset(&["locationID", "corporationID", "allianceID", "claimTime"], ids.iter().map(|id| {
        let row = by_system.get(id);
        vec![Value::Int(*id), optional(row.and_then(|r| r.claim_corporation_id)), optional(row.and_then(|r| r.alliance_id)), optional(row.and_then(|r| r.claim_time))]
    }).collect())
}

pub(crate) fn history(rows: &[HistoryRow]) -> Value {
    rowset(&["solarSystemID", "stationID", "oldOwnerID", "ownerID"], rows.iter().filter(|r| r.old_alliance_id != r.new_alliance_id).map(|r| vec![Value::Int(r.system_id), Value::None, optional(r.old_alliance_id), optional(r.new_alliance_id)]).collect())
}
