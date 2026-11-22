//! Pure tests for the paste formula, duration arithmetic and settlement claim.
use super::*;

#[test]
fn paste_units_follow_client_formula() {
    // FindNumUnitsToRepairModule: ceil(damage * base/hp * multiplier / paste base).
    assert_eq!(paste_units(2.5, 10.0, 65_000.0, 0.5, 6500.0).unwrap(), 2);
    assert_eq!(paste_units(1.0, 10.0, 65_000.0, 0.5, 6500.0).unwrap(), 1);
    assert_eq!(paste_units(2.5, 10.0, 0.0, 0.5, 6500.0).unwrap(), 0);
    assert_eq!(paste_units(0.0, 10.0, 65_000.0, 0.5, 6500.0).unwrap(), 0);
    assert_eq!(paste_units(-1.0, 10.0, 65_000.0, 0.5, 6500.0).unwrap(), 0);
    assert!(matches!(paste_units(f64::NAN, 10.0, 65_000.0, 0.5, 6500.0), Err(CallError::Internal(_))));
    assert!(matches!(paste_units(f64::NAN, 10.0, 0.0, 0.5, 6500.0), Err(CallError::Internal(_))));
    assert!(matches!(paste_units(1.0, 0.0, 65_000.0, 0.5, 6500.0), Err(CallError::Internal(_))));
    assert!(matches!(paste_units(1.0, -10.0, 65_000.0, 0.5, 6500.0), Err(CallError::Internal(_))));
    assert!(matches!(paste_units(1.0, 10.0, 65_000.0, 0.5, 0.0), Err(CallError::Internal(_))));
    assert!(matches!(paste_units(1.0, 10.0, 65_000.0, f64::NAN, 6500.0), Err(CallError::Internal(_))));
}

#[test]
fn duration_and_repaired_hp_are_proportional_and_capped() {
    assert_eq!(duration_ms(10.0, 10.0).unwrap(), 60_000);
    assert_eq!(duration_ms(0.3, 20.0).unwrap(), 900);
    assert_eq!(duration_ms(0.3, 10.0).unwrap(), 1_800);
    assert!(matches!(duration_ms(1.0, 0.0), Err(CallError::Internal(_))));
    assert!(matches!(duration_ms(1.0, -1.0), Err(CallError::Internal(_))));
    assert!(matches!(duration_ms(1.0, f64::NAN), Err(CallError::Internal(_))));
    assert_eq!(repaired_hp(10.0, 20.0, FILETIME_MINUTE), 10.0);
    assert_eq!(repaired_hp(30.0, 20.0, FILETIME_MINUTE), 20.0);
    assert_eq!(repaired_hp(30.0, 20.0, FILETIME_MINUTE / 2), 10.0);
    assert_eq!(repaired_hp(10.0, 20.0, 0), 0.0);
    assert_eq!(repaired_hp(10.0, 20.0, -FILETIME_MINUTE), 0.0);
}

#[test]
fn finish_claim_is_exclusive_and_token_fenced() {
    let (module, other) = (9_700_000_001, 9_700_000_002);
    let entry = |token| Repair { owner: 1, ship: 2, system: 3, started_at: 0, paid_hp: 1.0, rate_hp_per_min: 1.0, token };
    state().insert(module, entry(7));
    assert!(claim(module, Some(6)).is_none(), "a stale token cannot claim");
    assert!(repairing(module));
    assert!(claim(module, Some(7)).is_some(), "the due token claims once");
    assert!(claim(module, Some(7)).is_none());
    assert!(!repairing(module));
    assert!(claim(other, None).is_none(), "nothing to claim for another module");
    state().insert(module, entry(8));
    assert!(claim(module, None).is_some(), "an explicit stop claims any token");
    assert!(!repairing(module));
}
