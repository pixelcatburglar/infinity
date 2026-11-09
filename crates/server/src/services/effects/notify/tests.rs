use super::*;

#[test]
fn drone_notifications_preserve_client_arguments_and_none() {
    assert_eq!(drone_state(10, Some(20), Some(30), 6, 40, Some(50), Some(60)),
        vec![Value::Int(10), Value::Int(20), Value::Int(30), Value::Int(6), Value::Int(40), Value::Int(50), Value::Int(60)]);
    assert_eq!(drone_state(10, Some(20), None, 0, 40, None, None),
        vec![Value::Int(10), Value::Int(20), Value::None, Value::Int(0), Value::Int(40), Value::None, Value::None]);
    assert_eq!(drone_activity(10, Some(20), Some("assist")), vec![Value::Int(10), Value::Int(20), Value::str("assist")]);
    assert_eq!(drone_activity(10, None, None), vec![Value::Int(10), Value::None, Value::None]);
    assert_eq!(drone_control_lost(10), vec![Value::Int(10)]);
}

#[test]
fn ewar_notifications_use_client_string_slot_and_argument_order() {
    let args = vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::str("webify")];
    assert_eq!(ewar(1, 2, 3, "webify", true), ("OnEwarStart", args.clone()));
    assert_eq!(ewar(1, 2, 3, "webify", false), ("OnEwarEnd", args));
}

#[test]
fn jam_notifications_preserve_identity_filetime_and_milliseconds() {
    let identity = vec![Value::Int(11), Value::Int(22), Value::Int(33), Value::str("electronic")];
    let started = 133_000_000_000_000_000;
    let (name, args) = jam(11, 22, 33, true, started, 20_000.5);
    assert_eq!(name, "OnJamStart");
    assert_eq!(args.len(), 6);
    assert_eq!(&args[..4], identity.as_slice());
    assert_eq!(args[4], Value::Int(started));
    assert_eq!(args[5], Value::Float(20_000.5));
    let (name, args) = jam(11, 22, 33, false, started, 20_000.5);
    assert_eq!(name, "OnJamEnd");
    assert_eq!(args.len(), 4);
    assert_eq!(args, identity);
}

#[test]
fn npc_scram_slots_distinguish_mwd_blocking() {
    assert_eq!(ewar_slot_by_id(infinity_core::consts::dogma::EFFECT_NPC_WARP_SCRAMBLE), Some("warpScrambler"));
    assert_eq!(ewar_slot_by_id(infinity_core::consts::dogma::EFFECT_NPC_WARP_SCRAMBLE_MWD), Some("warpScramblerMWD"));
    assert_eq!(ewar_slot_by_id(infinity_core::consts::dogma::EFFECT_NPC_BEHAVIOR_WARP_SCRAMBLE), Some("warpScramblerMWD"));
}

#[test]
fn ship_effect_start_and_stop_keep_times_and_environment() {
    let charge = Value::tuple([Value::Int(30), Value::Int(27), Value::Int(55)]);
    let env = environment::build(10, 20, 30, Some(40), Some(charge), 60);
    for start in [true, false] {
        let args = ship_effect_args(10, 60, env.clone(), start, 100, 250.0, 3, 200);
        assert_eq!(args, vec![
            Value::Int(10), Value::Int(60), Value::Int(200), Value::Int(start as i64),
            Value::Int(start as i64), env.clone(), Value::Int(100), Value::Float(250.0),
            Value::Int(3), Value::None, if start { Value::None } else { Value::Int(200) },
        ]);
    }
}

#[test]
fn special_fx_has_all_client_arguments() {
    assert_eq!(fire_fx(100, 30, Some((10, 11)), "effects.Laser", Some(40), Some(55), true, true, 250.0, 3),
        event(100, "OnSpecialFX", vec![
            Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
            Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
            Value::Float(250.0), Value::Int(3), Value::None, Value::Int(0), Value::None,
        ]));
    assert_eq!(fire_fx(100, 30, None, "effects.Laser", None, None, false, false, 250.0, 0),
        event(100, "OnSpecialFX", vec![
            Value::Int(30), Value::None, Value::None, Value::None, Value::None,
            Value::str("effects.Laser"), Value::Bool(false), Value::Int(0), Value::Int(0),
            Value::Float(250.0), Value::Int(0), Value::None, Value::Int(0), Value::None,
        ]));
}

#[test]
fn scans_encode_client_rows_and_argument_order() {
    let rows = vec![(34, 7), (35, i64::MAX)];
    let encoded = Value::list([
        Value::tuple([Value::Int(34), Value::Int(7)]),
        Value::tuple([Value::Int(35), Value::Int(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT)]),
    ]);
    assert_eq!(cargo_scan_args(42, rows.clone()), vec![Value::Int(42), encoded.clone()]);
    assert_eq!(ship_scan_args(42, 1.5, 3.0, rows), vec![Value::Int(42), Value::Float(1.5), Value::Float(3.0), encoded]);
    assert_eq!(cargo_scan_args(42, vec![]), vec![Value::Int(42), Value::list([])]);
}

#[test]
fn special_fx_graphic_info_preserves_other_arguments() {
    let info = Value::dict([("targets", Value::list([Value::Int(40), Value::Int(41)]))]);
    assert_eq!(fire_fx_with_info(100, 30, Some((10, 11)), "effects.Laser", Some(40), Some(55), true, true, 250.0, 3, info.clone()),
        event(100, "OnSpecialFX", vec![
            Value::Int(30), Value::Int(10), Value::Int(11), Value::Int(40), Value::Int(55),
            Value::str("effects.Laser"), Value::Bool(true), Value::Int(1), Value::Int(1),
            Value::Float(250.0), Value::Int(3), Value::None, Value::Int(0), info,
        ]));
    assert_eq!(fire_fx(100, 30, None, "effects.Laser", None, None, false, false, 250.0, 0),
        fire_fx_with_info(100, 30, None, "effects.Laser", None, None, false, false, 250.0, 0, Value::None));
}

#[test]
fn attribute_change_preserves_sublocation_and_values() {
    let item = Value::tuple([Value::Int(30), Value::Int(27), Value::Int(55)]);
    let Value::Tuple(args) = attribute_change(20, item.clone(), 99, Value::Int(3), Value::Int(4)) else { panic!("expected attribute change tuple") };
    assert_eq!(args.len(), 8);
    assert_eq!(&args[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(20), item, Value::Int(99)]);
    assert!(matches!(args[4], Value::Int(_)));
    assert_eq!(&args[5..], &[Value::Int(3), Value::Int(4), Value::None]);
}
