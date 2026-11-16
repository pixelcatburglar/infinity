//! Pure precursor-turret ramp: bind identity, base/step/cap arithmetic, reset,
//! cap-arrival timestamp and the client's `graphicInfo` payload.
use super::*;
use marshal::Value;

const PARAMS: SpoolParameters = SpoolParameters { step: 0.05, maximum: 0.12 };
fn binding(seed: i64) -> SpoolBinding {
    SpoolBinding { source_membership: seed as u64, activation_generation: seed as u64, target: seed, target_membership: seed as u64 }
}

#[test]
fn ramp_binds_at_base_steps_saturates_and_resets() {
    let mut state = SpoolState::default();
    state.bind(binding(1));
    assert_eq!((state.binding(), state.current(PARAMS)), (Some(binding(1)), 0.0));
    state.advance(PARAMS);
    assert_eq!(state.current(PARAMS), PARAMS.step);
    for _ in 0..4 { state.advance(PARAMS); }
    assert_eq!(state.current(PARAMS), PARAMS.maximum);
    state.bind(binding(1));
    assert_eq!(state.current(PARAMS), PARAMS.maximum);
    state.bind(binding(2));
    state.advance(PARAMS);
    assert_eq!(state.current(PARAMS), PARAMS.step);
    state.reset();
    assert_eq!((state.binding(), state.current(PARAMS)), (None, 0.0));
}

#[test]
fn ramp_and_arrival_clamp_to_the_live_parameters() {
    let mut state = SpoolState::default();
    state.bind(binding(1));
    for _ in 0..4 { state.advance(PARAMS); }
    let shrunk = SpoolParameters { step: PARAMS.step, maximum: 0.07 };
    assert_eq!(state.current(shrunk), shrunk.maximum);
    assert_eq!(state.current(SpoolParameters { step: -1.0, maximum: -0.5 }), 0.0);
    let (start, cycle) = (1_000_000_i64, crate::client::filetime_ticks_from_ms(2_000.0));
    assert_eq!(PARAMS.maximum_at(start, 0.0, 2_000.0), start + 3 * cycle);
    assert_eq!(PARAMS.maximum_at(start, PARAMS.maximum, 2_000.0), start);
    for parameters in [SpoolParameters { step: 0.0, maximum: PARAMS.maximum }, SpoolParameters { step: PARAMS.step, maximum: 0.0 }] {
        assert_eq!(parameters.maximum_at(start, 0.0, 2_000.0), 0);
    }
}

#[test]
fn graphic_info_uses_the_client_wire_names() {
    let Value::Dict(entries) = graphic_info(PARAMS) else { panic!("graphicInfo is a dict") };
    let get = |name| entries.iter().find(|(key, _)| *key == Value::str(name)).map(|(_, value)| value.clone());
    assert_eq!((entries.len(), get("mulitplierBonusPerCycle"), get("multiplierBonusMax")),
        (2, Some(Value::Float(PARAMS.step)), Some(Value::Float(PARAMS.maximum))));
}
