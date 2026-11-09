//! Selector and empty-public-state behavior for the shared read kernel.
use crate::services::sovereignty::{queries, wire};
use crate::services::value_field;
use marshal::Value;

#[test]
fn selectors_reject_nonpositive_values_without_narrowing_valid_ids() {
    assert!(queries::positive(0).is_err());
    assert!(queries::positive(-1).is_err());
    assert_eq!(queries::positive(i64::MAX).unwrap(), i64::MAX);
}

#[test]
fn empty_public_and_scoped_views_have_iterable_headers() {
    let systems = wire::alliance_systems(&[]);
    assert_eq!(value_field(&systems, "header"), Some(&Value::list([Value::str("solarSystemID"), Value::str("allianceID")])));
    assert_eq!(value_field(&systems, "lines"), Some(&Value::list([])));
    let map = wire::current_map(&[], &[]);
    assert_eq!(value_field(&map, "lines"), Some(&Value::list([])));
    let capital = wire::capital(None);
    for name in ["currentCapitalSystem", "newCapitalSystem", "newCapitalSystemValidAfter"] {
        assert_eq!(value_field(&capital, name), Some(&Value::None));
    }
}
