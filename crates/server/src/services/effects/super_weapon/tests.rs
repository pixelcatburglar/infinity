use super::*;

#[test]
fn slash_axis_is_unit_normal_or_a_unit_perpendicular_fallback() {
    assert_eq!(slash_axis([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]);
    assert_eq!(slash_axis([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]);
    // Parallel or identical endpoints fall back to a unit perpendicular of d0.
    for d0 in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, 0.95, 0.2], [-1.0, -1.0, -1.0]] {
        let axis = slash_axis(d0, d0);
        let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-9, "d0 {d0:?}");
        assert!(dot(axis, d0).abs() < 1e-9, "d0 {d0:?} axis {axis:?}");
    }
}

#[test]
fn window_prefers_fitted_duration_over_aoe_fallback() {
    let inputs = |duration_ms, aoe_duration_ms| SuperWeaponInputs {
        warning_ms: 0.0, damage_delay_ms: 0.0, duration_ms, cycle_ms: 1.0, radius: 1.0,
        range: 1.0, signature_radius: 1.0, damage: [0.0; 4], aoe_duration_ms,
        neut_radius: 0.0, neut_amount: 0.0, neut_signature_radius: 0.0,
        immobility_ms: 0.0, no_jump_or_cloak_ms: 0.0, resistance_id: 0,
    };
    assert_eq!(window_ms(&inputs(8_000.0, 12_000.0)), 8_000.0);
    assert_eq!(window_ms(&inputs(0.0, 12_000.0)), 12_000.0);
    assert_eq!(window_ms(&inputs(-5.0, -1.0)), 0.0);
}
