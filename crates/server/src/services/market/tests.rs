use super::*;

mod broker_standings;

#[test]
fn station_preserves_location_and_defaults_null_owner() {
    let mut row = crate::sde::map::Station {
        station_id: 10, type_id: None, owner_id: None, solar_system_id: Some(20),
        constellation_id: Some(30), region_id: Some(40), operation_id: None,
        x: None, y: None, z: None, security: None,
    };
    let at = market_station(&row);
    assert_eq!((at.id, at.system, at.constellation, at.region, at.owner), (10, 20, 30, 40, 0));
    row.owner_id = Some(50);
    assert_eq!(market_station(&row).owner, 50);
}

#[test]
fn broker_fee_applies_minimum_and_rounds() {
    crate::tests::init_config();
    assert_eq!(broker_fee(0.03, 10.0), 100.0);
    assert_eq!(broker_fee(0.03, 4_001.0), 120.03);
}

#[test]
fn broker_standings_adjust_rate_without_clamping() {
    crate::tests::init_config();
    assert_eq!(broker_fee(adjusted_broker_rate(0.015, 3.0, 5.0), 1_000_000.0), 13_100.0);
    assert_eq!(broker_fee(adjusted_broker_rate(0.015, 0.0, 0.0), 1_000_000.0), 15_000.0);
    assert_eq!(broker_fee(adjusted_broker_rate(0.015, -3.0, -5.0), 1_000_000.0), 16_900.0);
    let negative = adjusted_broker_rate(0.001, 3.0, 5.0);
    assert!(negative < 0.0);
    assert_eq!(broker_fee(negative, 1_000_000.0), 100.0);
    assert_eq!(broker_fee(adjusted_broker_rate(0.015, 3.0, 5.0), 10_001.0), 131.01);
}

#[test]
fn duration_obeys_policy_and_timestamp_bounds() {
    crate::tests::init_config();
    assert!(valid_duration(90, 0));
    assert!(!valid_duration(91, 0));
    assert!(!valid_duration(-1, 0));
    assert!(!valid_duration(1, i64::MAX));
}
