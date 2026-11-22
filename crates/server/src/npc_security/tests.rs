//! Canonical multiplier, legacy factor and new-gain composition.
use super::*;

fn multiplier(value: f64) -> FrozenMultiplier { FrozenMultiplier::new(value).unwrap() }

fn close(actual: f64, expected: f64) {
    let scale = expected.abs().max(1.0);
    assert!((actual - expected).abs() <= 1e-12 * scale, "{actual} vs {expected}");
}

#[test]
fn legacy_factor_matches_original_settlement_formula() {
    assert_eq!(SecurityGain::for_legacy(0.0, 5), SecurityGain::ZERO);
    assert_eq!(SecurityGain::for_legacy(1.0, 5).fraction(), 1.0);
    assert_eq!(SecurityGain::for_legacy(0.5, 0), SecurityGain::ZERO);
    let (rate, count) = (1e-9_f64, 1_000_000_u64);
    let stable = SecurityGain::for_legacy(rate, count).fraction();
    let (mut reference, mut term, mut k) = (0.0_f64, count as f64 * rate, 1.0_f64);
    while term.abs() > 1e-20 * count as f64 * rate {
        reference += term;
        k += 1.0;
        term *= -(count as f64 - k + 1.0) / k * rate;
    }
    assert!(reference.is_finite() && reference != 0.0);
    close(stable, reference);
    // One unpaid legacy kill moves from rate 0.1 to settlement rate 0.2:
    // security zero becomes 2, not the deployment-rate result 1.
    let settled = SecurityGain::for_legacy(0.2, 1).fraction();
    close(0.0 + (10.0 - 0.0) * settled, 2.0);
    let shift = |value: f64, fraction: f64, toward: f64| value + (toward - value) * fraction;
    let interleaved = shift(shift(shift(0.0, 0.05, 10.0), 0.10, -10.0), 0.05, 10.0);
    let aggregated = shift(shift(0.0, SecurityGain::for_legacy(0.05, 2).fraction(), 10.0), 0.10, -10.0);
    assert!((interleaved - aggregated).abs() > 1e-9);
}

#[test]
fn mixed_gain_is_one_toward_limit_transformation() {
    let mixed = SecurityGain::for_legacy(0.2, 1).merge(SecurityGain::for_kill(0.25, multiplier(1.0)));
    close(mixed.fraction(), 1.0 - 0.8 * 0.75);
    assert_eq!(SecurityGain::for_kill(1.0, multiplier(0.0)), SecurityGain::ZERO);
    let full = SecurityGain::for_kill(1.0, multiplier(1.0));
    assert_eq!(full.storage(), None);
    assert_eq!(SecurityGain::from_storage(None), Some(full));
}

#[test]
fn full_and_zero_roundtrip_and_overflow_becomes_full() {
    assert!(FrozenMultiplier::new(f64::NAN).is_none());
    assert!(FrozenMultiplier::new(-1.0).is_none());
    assert_eq!(FrozenMultiplier::new(-0.0).unwrap().value().to_bits(), 0);
    assert_eq!(SecurityGain::ZERO.storage(), Some(0.0));
    let full = SecurityGain::for_kill(1.0, multiplier(1.0));
    assert_eq!(SecurityGain::for_kill(1.0, multiplier(0.0)), SecurityGain::ZERO);
    assert_eq!(full.storage(), None);
    assert_eq!(SecurityGain::from_storage(Some(0.0)), Some(SecurityGain::ZERO));
    assert!(SecurityGain::from_storage(Some(f64::INFINITY)).is_none());
    assert!(SecurityGain::from_storage(Some(-1.0)).is_none());
    let restored = SecurityGain::from_storage(full.merge(SecurityGain::for_kill(0.2, multiplier(1.0))).storage());
    assert_eq!(restored, Some(full));
    let huge = SecurityGain::from_storage(Some(f64::MAX)).unwrap();
    assert_eq!(huge.merge(SecurityGain::from_storage(Some(f64::MAX)).unwrap()).storage(), None);
}

#[test]
fn canonical_merge_compares_exact_storage_not_regrouped_operands() {
    let left = SecurityGain::for_kill(0.2, multiplier(0.5));
    let right = SecurityGain::for_kill(0.1, multiplier(1.0));
    let merged = left.merge(right);
    assert_eq!(SecurityGain::from_storage(merged.storage()), Some(merged));
    assert_ne!(merged.storage(), left.merge(right).merge(SecurityGain::ZERO).storage().map(|v| v + f64::EPSILON));
    close(merged.fraction(), 1.0 - (1.0 - 0.1) * (1.0 - 0.1));
}
