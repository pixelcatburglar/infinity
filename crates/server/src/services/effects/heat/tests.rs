
//! Pure and actor-rack tests for curve evolution, reset fencing, and damage policy.
//! Curve behavior mirrors `dogma/attributes/heatAttribute.py`.
//! Inputs use production Fit configuration where actor state is exercised.
//! Durable damage publication and the FIFO callback race are covered by focused tests.
use crate::client::filetime_from_unix_ms;
use crate::services::fit::{Fit, Key};
use crate::space::System;

#[cfg(any(feature = "tests-effects", feature = "tests-ships"))]
/// Seed actor heat through the same Fit-derived configuration as production.
pub(in crate::services) fn seed_ship_heat(s: &mut System, ship: i64, owner: i64, fit: &Fit, heat: [f64; 3], at: i64) {
    let snapshots = super::empty_ship_state(fit, filetime_from_unix_ms(at));
    let state = s.heat.ships.entry(ship).or_insert_with(|| super::Ship { owner, racks: std::array::from_fn(|_| super::Rack::default()) });
    state.owner = owner;
    for ((index, rack), (snapshot, value)) in state.racks.iter_mut().enumerate().zip(snapshots.into_iter().zip(heat)) {
        rack.heat = value;
        rack.capacity = snapshot.capacity;
        rack.incoming = snapshot.incoming;
        rack.generation = snapshot.generation;
        rack.dissipation = snapshot.dissipation;
        rack.attenuation = fit.value(Key::Ship, super::ATTENUATION[index]);
        rack.last = at;
        rack.anchor_heat = value;
        rack.anchor_at = at;
    }
}

#[cfg(feature = "tests-effects")] use super::rack::{curve, damage_chance};

#[cfg(feature = "tests-effects")]
#[test]
fn fixed_curve_references() {
    assert_eq!(curve(20.0, 100.0, 0.0, 1.0, 0.01, 1000.0).to_bits(), 0x4033cd0e1e3b421f);
    assert_eq!(curve(20.0, 100.0, 0.5, 0.5, 0.01, 1000.0).to_bits(), 0x4042d91479a85d72);
}

#[cfg(feature = "tests-effects")]
#[test]
fn cooling_cutoff_and_epsilon() {
    assert_eq!(curve(0.5, 100.0, 0.0, 1.0, 0.0, 0.0), 0.5);
    assert_eq!(curve(f64::from_bits(0.5_f64.to_bits() - 1), 100.0, 0.0, 1.0, 0.0, 0.0), 0.0);
    assert_eq!(curve(20.0, 100.0, 5e-8 - f64::EPSILON, 1.0, 0.01, 1000.0).to_bits(), 0x4033cd0e1e3b421f);
    assert!(curve(20.0, 100.0, 5e-8, 1.0, 0.01, 1000.0) > 20.0);
}

#[cfg(feature = "tests-effects")]
#[test]
fn curve_monotonic_and_bounded() {
    let a = curve(20.0, 100.0, 0.5, 0.5, 0.01, 1000.0);
    let b = curve(a, 100.0, 0.5, 0.5, 0.01, 1000.0);
    assert!(20.0 < a && a < b && b < 100.0);
    assert_eq!(curve(100.0, 100.0, 0.5, 0.5, 0.01, 1000.0), 100.0);
    assert!(curve(20.0, 100.0, 0.0, 1.0, 0.01, 2000.0) < 20.0);
}

#[cfg(feature = "tests-effects")]
#[test]
fn published_chance_and_physical_distance() {
    let chance = damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 0, true);
    assert_eq!(chance.to_bits(), 0.4714285714285714_f64.to_bits());
    assert_eq!(damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 1, true), chance * 0.5);
    assert_eq!(damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 1, false), 0.0);
    assert_eq!(damage_chance(60.0, 100.0, 11, 14, 0.5, 0, 2, true), chance * 0.25);
    assert_eq!(damage_chance(60.0, 0.0, 11, 14, 0.5, 0, 0, true), 0.0);
    assert_eq!(damage_chance(60.0, 100.0, 11, 0, 0.5, 0, 0, true), 0.0);
}


#[cfg(feature = "tests-effects")] mod early_clear;
#[cfg(feature = "tests-effects")] mod reset;
