//! Recovery timing policy defaults, TOML mapping and bound validation.

use super::*;

#[test]
fn defaults_validate_and_toml_maps() {
    let defaults = RepairableShipConfig::default();
    assert_eq!((defaults.payment_retention_days, defaults.delivery_seconds, defaults.withdrawal_retention_days), (30, 3600, 30));
    defaults.validate().unwrap();
    let parsed: RepairableShipConfig = toml::from_str(
        "payment_retention_days = 7\ndelivery_seconds = 60\nwithdrawal_retention_days = 14",
    ).unwrap();
    assert_eq!((parsed.payment_retention_days, parsed.delivery_seconds, parsed.withdrawal_retention_days), (7, 60, 14));
    parsed.validate().unwrap();
    assert!(toml::from_str::<RepairableShipConfig>("unknown_key = 1").is_err());
}

#[test]
fn nonpositive_and_overflowing_durations_reject() {
    for field in ["payment_retention_days", "delivery_seconds", "withdrawal_retention_days"] {
        for value in [0i64, -1, i64::MAX] {
            let config: RepairableShipConfig = toml::from_str(&format!("{field} = {value}")).unwrap();
            let expected = if value > 0 {
                format!("repairable_ship.{field}={value} exceeds the representable duration or deadline")
            } else {
                format!("repairable_ship.{field} must be positive")
            };
            assert_eq!(config.validate().unwrap_err().to_string(), expected, "{field}={value}");
        }
    }
}
