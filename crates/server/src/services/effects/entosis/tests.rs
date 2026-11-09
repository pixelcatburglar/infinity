//! Pure validation of effective Entosis cycle parameters; no database or actor fixture.
use super::valid_parameters;

#[test]
fn cycle_values_must_be_finite_and_duration_positive() {
    assert_eq!(valid_parameters(5000.0, 25000.0), Some((5000.0, 25000.0)));
    assert_eq!(valid_parameters(1.0, 0.0), Some((1.0, 0.0)));
    for (duration, range) in [(0.0, 1.0), (-1.0, 1.0), (f64::NAN, 1.0),
        (f64::INFINITY, 1.0), (1.0, -1.0), (1.0, f64::NAN), (1.0, f64::INFINITY)] {
        assert_eq!(valid_parameters(duration, range), None);
    }
}


#[test]
fn warmup_and_renewal_preserve_the_original_source_proof() {
    use crate::{services::sovereignty::CaptureContributor, space::sovereignty::links::LinkCycle};
    use std::sync::{Arc, atomic::AtomicBool};
    let mut proof = LinkCycle {
        target_item_id: 1, target_epoch: 2,
        contributor: CaptureContributor { character_id: 3, ship_id: 4, module_id: 5,
            corporation_id: 6, alliance_id: 7, source_revision: 8 },
        membership_generation: 9, activation: Arc::new(AtomicBool::new(false)),
        range_m: 1.0, scoring: true, expires_sim: 50,
    };
    super::next_cycle(&mut proof, 0, 100.0);
    assert!(!proof.scoring);
    assert_eq!(proof.expires_sim, 0);
    super::next_cycle(&mut proof, 1, 200.0);
    assert!(proof.scoring);
    assert_eq!(proof.range_m, 200.0);
    assert_eq!(proof.target_epoch, 2);
    assert_eq!(proof.membership_generation, 9);
    assert_eq!(proof.contributor.source_revision, 8);
    assert_eq!(proof.contributor.corporation_id, 6);
}