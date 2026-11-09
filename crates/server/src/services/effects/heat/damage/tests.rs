use super::*;

#[test]
fn burnout_requires_positive_finite_health_and_exhausted_damage() {
    assert!(!exhausted(10.0, 9.999));
    assert!(exhausted(10.0, 10.0));
    assert!(exhausted(10.0, 11.0));
    assert!(!exhausted(0.0, 0.0));
    assert!(!exhausted(-1.0, 0.0));
    assert!(!exhausted(f64::INFINITY, f64::INFINITY));
    assert!(!exhausted(f64::NAN, 10.0));
    assert!(!exhausted(10.0, f64::NAN));
}


#[test]
fn injury_roll_and_clamping_are_deterministic() {
    assert_eq!(injury(4.0, 10.0, 3.0, 0.5, 0.49), 7.0);
    assert_eq!(injury(4.0, 10.0, 3.0, 0.5, 0.5), 4.0);
    assert_eq!(injury(9.0, 10.0, 3.0, 1.0, 0.0), 10.0);
    assert_eq!(injury(10.0, 10.0, 3.0, 1.0, 0.0), 10.0);
    assert_eq!(injury(4.0, 10.0, 3.0, f64::NAN, 0.0), 4.0);
    let first = injury(2.0, 10.0, 3.0, 1.0, 0.2);
    let second = injury(first, 10.0, 7.0, 1.0, 0.2);
    assert_eq!((first, second), (5.0, 10.0));
}
#[test]
fn published_chance_includes_offline_slot_distance() {
    let chance = |slot, online| super::super::rack::damage_chance(60.0, 100.0, 11, 14, 0.5, 0, slot, online);
    assert_eq!(chance(0, true).to_bits(), (0.6_f64 * (11.0 / 14.0)).to_bits());
    assert_eq!(chance(1, true), chance(0, true) * 0.5);
    assert_eq!(chance(2, true), chance(0, true) * 0.25);
    assert_eq!(chance(1, false), 0.0);
    assert_eq!(chance(2, true), 0.11785714285714285);
}

#[test]
fn burnout_reduces_later_chances_and_burnt_source_still_deals_owed_damage() {
    use infinity_core::consts::FLAG_FITTING_FIRST;
    use std::sync::atomic::AtomicBool;

    let flag = FLAG_FITTING_FIRST;
    let source = |module| super::super::SourceSnapshot {
        module, flag, heat: 1.0, master: module, capacity: 1.0,
        attenuation: 1.0, heat_damage: 1.0, generation: Arc::new(AtomicBool::new(false)),
    };
    let sources = [source(1), source(2)];
    let mut damage = [
        (1, flag, None, 0.0, 10.0),
        (2, flag + 1, None, 0.0, 1.0),
        (3, flag + 2, None, 0.0, 10.0),
    ];
    let rolls = [
        (1, 1, 0.99), (1, 2, 0.0), (1, 3, 0.6),
        (2, 1, 0.3), (2, 2, 0.0), (2, 3, 0.3),
    ];
    accumulate(&sources, &mut damage, &rolls, 4).unwrap();
    assert_eq!([damage[0].3, damage[1].3, damage[2].3], [1.0, 1.0, 1.0]);
}
