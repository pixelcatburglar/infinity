//! Resonance floor coverage. Fit::value bounds the twelve damage resonance
//! attributes below by combat.resonance_floor; modifier_value keeps raw
//! results, so operand reads in brain compilation and projected snapshots are
//! never floored before they multiply their targets; absent nodes and
//! attributes keep the raw zero result.

use super::super::{apply, bounded, Fit, Key, OP_PRE_MUL};
use crate::config::game::world::CombatConfig;
use crate::sde::types::tests::combat_fixture;
use infinity_core::consts::dogma::*;
use std::sync::Arc;

/// dogma/const.py attributeHull*DamageResonance: operand attributes consumed by
/// hull modifier rows such as emergencyHullEnergizer; deliberately outside the
/// bounded twelve so source reads keep raw values.
const HULL_OPERANDS: [i32; 4] = [974, 975, 976, 977];

/// The bounded set: structure 109/110/111/113, armor 267-270, shield 271-274.
const RESONANCE: [i32; 12] = [
    ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE,
    ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE,
    ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE,
    ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE,
    ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE,
    ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE,
];

fn floor() -> f64 { crate::tests::init_config().combat.resonance_floor }

/// A fixture hull carrying a drone whose override pins `attr` to `value`.
fn fit_with(attr: i32, value: f64) -> Fit {
    let mut fit = Fit::for_space_type(combat_fixture(), 1, Vec::new());
    fit.include_drone(1, 1, vec![(attr, value)]);
    fit
}

#[test]
fn floor_default_and_domain_validation() {
    assert_eq!(CombatConfig::default().resonance_floor, 0.05);
    assert_eq!(floor(), 0.05);
    let mut config = CombatConfig::default();
    for (candidate, ok) in [(0.05, true), (1.0, true), (f64::MIN_POSITIVE, true),
        (0.0, false), (-0.5, false), (1.5, false),
        (f64::NAN, false), (f64::INFINITY, false), (f64::NEG_INFINITY, false)] {
        config.resonance_floor = candidate;
        assert_eq!(config.validate().is_ok(), ok, "resonance_floor={candidate}");
    }
}

#[test]
fn all_twelve_resonances_floor_only_at_the_boundary() {
    let floor = floor();
    for attr in RESONANCE {
        let fit = fit_with(attr, 0.01);
        assert_eq!(fit.value(Key::Drone(1), attr), floor, "attr {attr} value");
        assert_eq!(fit.modifier_value(Key::Drone(1), attr), 0.01, "attr {attr} operand");
        // The fixture hull's 1.0 resonance stays above the floor untouched.
        assert_eq!(fit.value(Key::Ship, attr), 1.0, "attr {attr} hull");
    }
}

#[test]
fn vanquisher_style_compound_floors_once_at_the_boundary() {
    // Retail Vanquisher: two 0.01 resonance factors compound to 0.0001 inside
    // the guarded evaluator. The boundary raises the published resonance to
    // the floor instead of letting the raw product reach consumers.
    let floor = floor();
    let raw = apply(0.01, &[(OP_PRE_MUL, 0.01, false)]);
    assert!(raw > 0.0 && raw < floor, "raw compound {raw}");
    for attr in RESONANCE {
        assert_eq!(bounded(attr, raw), floor, "attr {attr}");
        assert_eq!(bounded(attr, 1.0), 1.0, "attr {attr} above floor");
    }
    // A non-resonance attribute keeps the compounded value raw.
    assert_eq!(bounded(ATTR_ARMOR_HP, raw), raw);
}

#[test]
fn exact_floor_and_above_pass_unchanged() {
    for base in [floor(), 0.5, 1.0] {
        let fit = fit_with(ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, base);
        assert_eq!(fit.value(Key::Drone(1), ATTR_STRUCTURE_EM_DAMAGE_RESONANCE), base, "{base}");
    }
}

#[test]
fn finite_nonpositive_clamps_while_nonfinite_passes_unchanged() {
    const ATTR: i32 = ATTR_SHIELD_EM_DAMAGE_RESONANCE;
    for (base, expected) in [(0.0, floor()), (-0.5, floor()),
        (f64::INFINITY, f64::INFINITY), (f64::NEG_INFINITY, f64::NEG_INFINITY)] {
        assert_eq!(fit_with(ATTR, base).value(Key::Drone(1), ATTR), expected, "{base}");
    }
    assert!(fit_with(ATTR, f64::NAN).value(Key::Drone(1), ATTR).is_nan());
}

#[test]
fn absent_resonance_and_missing_node_keep_the_raw_zero() {
    // An existing hull whose type never authored the attribute, and a missing
    // node, both keep the historical 0.0; the floor never synthesizes a value.
    let fit = Fit::for_space_type(combat_fixture(), 999, Vec::new());
    for attr in RESONANCE {
        assert_eq!(fit.value(Key::Ship, attr), 0.0, "attr {attr} untyped hull");
        assert_eq!(fit.value(Key::Char, attr), 0.0, "attr {attr} absent char");
        assert_eq!(fit.value(Key::Drone(1), attr), 0.0, "attr {attr} absent drone");
    }
    // Even an empty-SDE hull stays zero. Absence is not a floor candidate.
    let bare = Fit::for_space_type(Arc::new(crate::sde::Sde::default()), 1, Vec::new());
    assert_eq!(bare.value(Key::Ship, ATTR_SHIELD_EM_DAMAGE_RESONANCE), 0.0);
    assert_eq!(bare.modifier_value(Key::Ship, ATTR_SHIELD_EM_DAMAGE_RESONANCE), 0.0);
}

#[test]
fn adjacent_attribute_ids_are_not_bounded() {
    // Only the exact twelve are bounded; neighbours 112, 266 and 275 keep raw
    // sub-floor values, proving the set is attribute-exact, not a range.
    for attr in [112, 266, 275] {
        let fit = fit_with(attr, 0.01);
        assert_eq!(fit.value(Key::Drone(1), attr), 0.01, "attr {attr}");
        assert_eq!(fit.modifier_value(Key::Drone(1), attr), 0.01, "attr {attr}");
    }
}

#[test]
fn operand_attributes_and_non_resonance_values_stay_raw() {
    // 974-977 hull operands and ordinary attributes never reach the floor.
    for attr in HULL_OPERANDS.into_iter().chain([ATTR_ARMOR_HP]) {
        let fit = fit_with(attr, 0.01);
        assert_eq!(fit.value(Key::Drone(1), attr), 0.01, "attr {attr} value");
        assert_eq!(fit.modifier_value(Key::Drone(1), attr), 0.01, "attr {attr} operand");
    }
}

#[test]
fn premature_operand_clamp_would_change_modifier_arithmetic() {
    // Counterexample for flooring operands: a raw 0.01 operand scales a target
    // five times harder than a floored 0.05 operand would.
    crate::tests::init_config();
    let fit = fit_with(ATTR_ARMOR_EM_DAMAGE_RESONANCE, 0.01);
    let operand = fit.modifier_value(Key::Drone(1), ATTR_ARMOR_EM_DAMAGE_RESONANCE);
    assert_eq!(operand, 0.01);
    assert_eq!(apply(10.0, &[(OP_PRE_MUL, operand, false)]), 0.1);
    assert_eq!(apply(10.0, &[(OP_PRE_MUL, fit.value(Key::Drone(1), ATTR_ARMOR_EM_DAMAGE_RESONANCE), false)]), 0.5);
}

#[test]
fn memo_holds_raw_results_through_interleaved_reads() {
    // The memo stores the raw evaluation: boundary and operand reads share it
    // without polluting each other.
    let floor = floor();
    let attr = ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE;
    let fit = fit_with(attr, 0.01);
    for _ in 0..2 {
        assert_eq!(fit.value(Key::Drone(1), attr), floor);
        assert_eq!(fit.modifier_value(Key::Drone(1), attr), 0.01);
    }
    // Interleaving in the opposite order sees the same split.
    let other = fit_with(attr, 0.4);
    assert_eq!(other.modifier_value(Key::Drone(1), attr), 0.4);
    assert_eq!(other.value(Key::Drone(1), attr), 0.4);
}
