//! OnSpecialFX wire regression for the shared space/mod.rs encoder, the only
//! assembly; literal expected tuples freeze all fourteen client arguments.

use super::*;

#[test]
fn special_fx_encodes_all_client_arguments() {
    let actual = special_fx(100, 30, Some(FxModule { item: Some(10), type_id: 11 }), Some(40), Some(55),
        "effects.Laser", true, 1, 1, Value::Float(250.0), 3, None, 0, Value::None);
    let expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
        Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
        Value::Float(250.0), Value::Int(3), Value::None, Value::Int(0), Value::None]);
    assert_eq!(marshal::dumps(&actual), marshal::dumps(&expected));
    let actual = special_fx(100, 30, None, None, None, "effects.Laser", false, 0, 0,
        Value::Float(250.0), 0, None, 0, Value::None);
    let expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::None, Value::None, Value::None, Value::None,
        Value::str("effects.Laser"), Value::Bool(false), Value::Int(0), Value::Int(0),
        Value::Float(250.0), Value::Int(0), Value::None, Value::Int(0), Value::None]);
    assert_eq!(marshal::dumps(&actual), marshal::dumps(&expected));
}

#[test]
fn special_fx_graphic_info_preserves_other_arguments() {
    let info = Value::dict([("targets", Value::list([Value::Int(40), Value::Int(41)]))]);
    let actual = special_fx(100, 30, Some(FxModule { item: Some(10), type_id: 11 }), Some(40), Some(55),
        "effects.Laser", true, 1, 1, Value::Float(250.0), 3, None, 0, info.clone());
    let expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
        Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
        Value::Float(250.0), Value::Int(3), Value::None, Value::Int(0), info]);
    assert_eq!(marshal::dumps(&actual), marshal::dumps(&expected));
}

#[test]
fn special_fx_duration_type_and_independent_start_active() {
    let space = special_fx(100, 30, None, None, None, "effects.Warping", false, 1, 0,
        Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None);
    let space_expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::None, Value::None, Value::None, Value::None, Value::str("effects.Warping"),
        Value::Bool(false), Value::Int(1), Value::Int(0), Value::Int(-1), Value::Int(0), Value::None, Value::Int(0), Value::None]);
    let module = special_fx(100, 30, Some(FxModule { item: None, type_id: 11 }), None, None,
        "effects.Laser", false, 1, 1, Value::Float(-1.0), 0, None, 0, Value::None);
    let module_expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::None, Value::Int(11), Value::None, Value::None, Value::str("effects.Laser"),
        Value::Bool(false), Value::Int(1), Value::Int(1), Value::Float(-1.0), Value::Int(0), Value::None, Value::Int(0), Value::None]);
    assert_eq!(marshal::dumps(&space), marshal::dumps(&space_expected));
    assert_eq!(marshal::dumps(&module), marshal::dumps(&module_expected));
    let Value::Tuple(mut float_duration) = space.clone() else { panic!("event") };
    let Value::Tuple(body) = &mut float_duration[1] else { panic!("body") };
    let Value::Tuple(args) = &mut body[1] else { panic!("args") };
    args[9] = Value::Float(-1.0);
    assert_ne!(marshal::dumps(&space), marshal::dumps(&Value::Tuple(float_duration)));
}

#[test]
fn special_fx_encodes_start_time_phase_and_graphic_info() {
    let info = Value::dict([("targets", Value::list([Value::Int(40)]))]);
    let actual = special_fx(100, 30, Some(FxModule { item: Some(10), type_id: 11 }), Some(40), Some(55),
        "effects.Laser", true, 1, 1, Value::Float(250.0), 2, Some(900), 7, info.clone());
    let expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
        Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
        Value::Float(250.0), Value::Int(2), Value::Int(900), Value::Int(7), info]);
    assert_eq!(marshal::dumps(&actual), marshal::dumps(&expected));
    let fresh = special_fx(100, 30, Some(FxModule { item: Some(10), type_id: 11 }), Some(40), Some(55),
        "effects.Laser", true, 1, 1, Value::Float(250.0), 3, None, 0, Value::None);
    let fresh_expected = event(100, "OnSpecialFX", vec![
        Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
        Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
        Value::Float(250.0), Value::Int(3), Value::None, Value::Int(0), Value::None]);
    assert_eq!(marshal::dumps(&fresh), marshal::dumps(&fresh_expected));
}

#[test]
fn super_weapon_guid_selects_family_and_race() {
    use crate::space::system::super_weapons::{super_weapon_guid, SuperWeaponFxFamily};
    for (family, expected) in [
        (SuperWeaponFxFamily::Single, "effects.SuperWeapon"),
        (SuperWeaponFxFamily::DamageBeam, "effects.SWDamageBeam"),
        (SuperWeaponFxFamily::SlashBeam, "effects.SWSlashBeam"),
    ] {
        for (race, suffix) in [(4, "Amarr"), (1, "Caldari"), (8, "Gallente"), (2, "Minmatar")] {
            assert_eq!(super_weapon_guid(family, Some(race)), Some(format!("{expected}{suffix}").as_str()));
        }
        assert_eq!(super_weapon_guid(family, None), None);
        assert_eq!(super_weapon_guid(family, Some(3)), None);
    }
    // The cone family is race-free: every race maps to the same GUID.
    for race in [None, Some(1), Some(3)] {
        assert_eq!(super_weapon_guid(SuperWeaponFxFamily::Cone, race), Some("effects.SuperWeaponCone"));
    }
}

#[test]
fn super_weapon_graphic_info_carries_exact_keys_and_units() {
    use crate::space::system::super_weapons::{directional_weapon_graphic_info, slash_weapon_graphic_info};
    let vector = |v: V3| Value::tuple(v.into_iter().map(Value::Float).collect::<Vec<_>>());
    let expected = crate::services::keyval([
        ("targetOffset", vector([1.0, 2.0, 3.0])),
        ("duration", Value::Float(5_000.0)),
    ]);
    assert_eq!(marshal::dumps(&directional_weapon_graphic_info([1.0, 2.0, 3.0], 5_000.0)), marshal::dumps(&expected));
    let expected = crate::services::keyval([
        ("startTargetOffset", vector([1.0, 2.0, 3.0])),
        ("endTargetOffset", vector([4.0, 5.0, 6.0])),
        ("duration", Value::Float(12_000.0)),
    ]);
    assert_eq!(marshal::dumps(&slash_weapon_graphic_info([1.0, 2.0, 3.0], [4.0, 5.0, 6.0], 12_000.0)), marshal::dumps(&expected));
}
