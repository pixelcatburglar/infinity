//! `combat.resonance_floor` loading, domain validation and snapshot tests.

use super::*;

#[test]
fn resonance_floor_defaults_and_survives_unrelated_combat_keys() {
    let fixture = Fixture::new();
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().combat.resonance_floor, 0.05);
    fixture.write("[combat]\nbounty_multiplier = 2.0");
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().combat.resonance_floor, 0.05);
}

#[test]
fn resonance_floor_accepts_custom_values_into_the_game_snapshot() {
    let fixture = Fixture::new();
    for value in [0.001, 0.25, 1.0] {
        fixture.write(&format!("[combat]\nresonance_floor = {value}"));
        let config = Config::load_with(&fixture.path(), |_| None).unwrap();
        assert_eq!(config.combat.resonance_floor, value);
        assert_eq!(config.game_config().combat.resonance_floor, value);
    }
    // An integer literal is a boundary value, not a type error.
    fixture.write("[combat]\nresonance_floor = 1");
    assert_eq!(Config::load_with(&fixture.path(), |_| None).unwrap().combat.resonance_floor, 1.0);
}

#[test]
fn resonance_floor_has_no_environment_override() {
    let fixture = Fixture::new();
    fixture.write("[combat]\nresonance_floor = 0.1");
    let config = Config::load_with(&fixture.path(), |key| (key == "COMBAT_RESONANCE_FLOOR").then(|| "0.9".into())).unwrap();
    assert_eq!(config.combat.resonance_floor, 0.1);
}

#[test]
fn resonance_floor_rejects_out_of_domain_values_by_dotted_key_and_value() {
    for value in ["0", "0.0", "-0.001", "1.0000001", "2", "nan", "-nan", "inf", "-inf"] {
        assert!(load_error(&format!("[combat]\nresonance_floor = {value}")).contains("combat.resonance_floor"), "{value}");
    }
    // The refusal names the offending value, not only the key.
    assert!(load_error("[combat]\nresonance_floor = 1.5").contains("combat.resonance_floor=1.5"));
}

#[test]
fn resonance_floor_rejects_a_wrong_type_like_every_key() {
    assert!(load_error("[combat]\nresonance_floor = '0.05'").contains("resonance_floor"));
}
