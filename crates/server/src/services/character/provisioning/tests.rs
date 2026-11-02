use super::*;

#[test]
fn unsupported_ship_has_no_rookie_fit() {
    assert!(rookie_fit::for_ship(-1).is_none());
}
