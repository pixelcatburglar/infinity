//! Synthetic committed rows exercise client-visible sovereignty projections.
use crate::{dao::sovereignty::{history::HistoryRow, policy::PolicyRow, structures::{HubRow, StructureRow}, systems::SystemRow}, services::{sovereignty::wire, value_field}};
use marshal::Value;

fn system(claimed: bool) -> SystemRow {
    SystemRow { system_id: 42, hub_item_id: Some(1_000_000_000_123), claim_corporation_id: claimed.then_some(8), alliance_id: claimed.then_some(9), claim_time: claimed.then_some(123456), ownership_epoch: 1, revision: 1 }
}
fn hub(claimed: bool) -> HubRow {
    HubRow { system: system(claimed), structure: StructureRow { item_id: 1_000_000_000_123, system_id: 42, source_item_id: 10, deployer_character_id: 11, deployer_corporation_id: 8, deployed_at: 12, content_namespace: "test".into(), content_version: "test".into(), self_destruct_due_at: None, self_destruct_actor: None }, type_id: 1, owner_id: 8, x: None, y: None, z: None }
}
fn policy() -> PolicyRow { PolicyRow { alliance_id: 9, current_capital_system: Some(42), last_capital_changed_at: None, capital_lost_at: None, capital_change_not_before: None, revision: 1 } }
fn roundtrip(v: &Value) -> Value { marshal::loads(&marshal::dumps(v)).unwrap() }

#[test]
fn constructors_use_real_claim_time_and_corporation() {
    let row = system(true);
    for (actual, expected) in [
        (wire::system_claim(&row), Value::reduce("sovereignty.data_types.SovClaimInfo", vec![Value::Int(1_000_000_000_123), Value::Int(8), Value::Int(9)], None)),
        (wire::hub(Some(&row)), Value::reduce("sovereignty.data_types.SovHubInfo", vec![Value::Int(1_000_000_000_123), Value::Int(8), Value::Int(9), Value::Int(123456)], None)),
    ] { assert_eq!(roundtrip(&actual), roundtrip(&expected)); }
    assert_eq!(wire::system_claim(&system(false)), Value::None);
    assert_eq!(wire::hub(Some(&system(false))), Value::None);
}

#[test]
fn neutral_structure_remains_visible_and_capital_is_real() {
    let neutral = wire::structures(&[hub(false)], &[policy()]);
    assert_eq!(neutral.len(), 1);
    assert_eq!(value_field(&neutral[0], "itemID"), Some(&Value::Int(1_000_000_000_123)));
    assert_eq!(value_field(&neutral[0], "allianceID"), Some(&Value::None));
    assert_eq!(value_field(&neutral[0], "isCapital"), Some(&Value::Bool(false)));
    let claimed = wire::structures(&[hub(true)], &[policy()]);
    assert_eq!(value_field(&claimed[0], "corporationID"), Some(&Value::Int(8)));
    assert_eq!(value_field(&claimed[0], "isCapital"), Some(&Value::Bool(true)));
    assert_eq!(value_field(&claimed[0], "campaignState"), Some(&Value::None));
    assert_eq!(value_field(&wire::capital(Some(&policy())), "currentCapitalSystem"), Some(&Value::Int(42)));
    assert_eq!(value_field(&wire::capital(None), "newCapitalSystemValidAfter"), Some(&Value::None));
}

#[test]
fn alliance_tuple_contains_shared_wide_id_and_empty_scores() {
    let value = wire::alliance_structures(&[hub(true)]);
    let Value::Tuple(parts) = value else { panic!("expected three projections") };
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0], parts[1]);
    let Value::Reduce { data, .. } = &parts[0] else { panic!("expected CRowset") };
    let args = data.items().unwrap()[1].items().unwrap();
    let rows = args[1].items().unwrap();
    let descriptor = marshal::DbRowDescriptor::new([
        ("structureID", marshal::DbType::I8), ("solarSystemID", marshal::DbType::I8), ("corporationID", marshal::DbType::I8),
        ("campaignStartTime", marshal::DbType::FileTime), ("campaignEventType", marshal::DbType::I4),
        ("campaignOccupancyLevel", marshal::DbType::R8), ("vulnerableStartTime", marshal::DbType::FileTime),
        ("vulnerableEndTime", marshal::DbType::FileTime), ("vulnerabilityOccupancyLevel", marshal::DbType::R8),
    ]);
    assert_eq!(descriptor.get(&rows[0], "structureID"), Some(Value::Int(1_000_000_000_123)));
    assert_eq!(descriptor.get(&rows[0], "campaignStartTime"), Some(Value::None));
    let Value::Reduce { data, .. } = &parts[2] else { panic!("expected score CRowset") };
    let args = data.items().unwrap()[1].items().unwrap();
    assert_eq!(args[1], Value::list([]));
}

#[test]
fn public_rows_include_unclaimed_and_history_only_alliance_changes() {
    let map = wire::current_map(&[42, 43], &[system(true)]);
    assert_eq!(value_field(&map, "header"), Some(&Value::list([Value::str("locationID"), Value::str("corporationID"), Value::str("allianceID"), Value::str("claimTime")])));
    assert_eq!(value_field(&map, "lines"), Some(&Value::list([Value::list([Value::Int(42), Value::Int(8), Value::Int(9), Value::Int(123456)]), Value::list([Value::Int(43), Value::None, Value::None, Value::None])])));
    let history = HistoryRow { history_id: 1, system_id: 42, hub_item_id: 1, old_corporation_id: None, new_corporation_id: Some(8), old_alliance_id: None, new_alliance_id: Some(9), old_epoch: 0, new_epoch: 1, reason: "claim".into(), at: 1, actor_id: None, operation_key: "test".into(), content_namespace: "test".into(), content_version: "test".into() };
    let mut unchanged = history.clone(); unchanged.old_alliance_id = Some(9);
    let result = wire::history(&[history, unchanged]);
    assert_eq!(value_field(&result, "lines"), Some(&Value::list([Value::list([Value::Int(42), Value::None, Value::None, Value::Int(9)])])));
}
