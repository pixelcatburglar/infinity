use super::*;

#[test]
fn operations_disabled_contract_and_ownership() {
    let kwargs = Value::Dict(vec![]);
    for (method, expected) in [
        ("get_character_progress", Value::Dict(vec![])),
        ("get_active_category_id", Value::None),
        ("get_operations_completed_at_least_once", Value::list([])),
        ("get_mission_avoidance_systems", Value::list([])),
        ("skip_task", Value::Bool(false)),
        ("activate_operation_in_solar_system", Value::Bool(false)),
        ("is_category_complete", Value::Bool(false)),
    ] {
        let call = Call { service: "operationsManager", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let value = root(&call).unwrap().unwrap();
        assert_eq!(value, expected, "{method}");
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        let other = Call { service: "other", ..call };
        assert!(root(&other).is_none());
    }
    for method in ["get_tutorial_state", "is_main_tutorial_finished", "has_skipped_tutorial", "start_site", "complete_category_for_character", "block_task", "unblock_task", "get_category_state", "process_client_event"] {
        let call = Call { service: "operationsManager", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let super::super::super::CallError::User { msg, dict } = root(&call).unwrap().unwrap_err() else { panic!("expected refusal for {method}") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::npe::OPERATIONS_UNAVAILABLE))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        let other = Call { service: "other", ..call };
        assert!(root(&other).is_none());
    }
    let eligibility = Call { service: "operationsManager", method: "can_character_play_the_tutorial", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(station(&eligibility).unwrap().unwrap(), Value::Bool(false));
    assert!(root(&eligibility).is_none());
    let unknown = Call { method: "unknown", ..eligibility };
    assert!(root(&unknown).is_none());
    assert!(station(&unknown).is_none());
    let air = Call { service: "air_npe", method: "is_air_npe_enabled", ..unknown };
    assert_eq!(root(&air).unwrap().unwrap(), Value::Bool(false));
}
