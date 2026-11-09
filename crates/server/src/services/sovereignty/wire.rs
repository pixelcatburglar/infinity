//! Sovereignty projections for `sovereignty.data_types` and `sovSvc`.
//! Values come from committed Hub, system, policy and history rows. Campaigns
//! and development indices are not modelled here.
use marshal::{DbRowDescriptor, DbType, Value};
use crate::{dao::sovereignty::{history::HistoryRow, policy::PolicyRow, structures::HubRow, systems::SystemRow}, services::{crowset, keyval, rowset}};

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
pub(crate) fn structures(hubs: &[HubRow], policies: &[PolicyRow]) -> Vec<Value> {
    hubs.iter().map(|h| {
        let capital = h.system.alliance_id.is_some_and(|alliance| policies.iter().any(|p| p.alliance_id == alliance && p.current_capital_system == Some(h.system.system_id)));
        keyval([
            ("itemID", Value::Int(h.structure.item_id)), ("typeID", Value::Int(i64::from(h.type_id))),
            ("solarSystemID", Value::Int(h.system.system_id)), ("corporationID", Value::Int(h.owner_id)),
            ("ownerID", Value::Int(h.owner_id)), ("allianceID", optional(h.system.alliance_id)),
            ("isCapital", Value::Bool(capital)), ("campaignState", Value::None), ("vulnerabilityState", Value::None),
        ])
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
