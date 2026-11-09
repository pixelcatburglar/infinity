use super::*;

#[test]
fn closed_survey_and_pvp_own_only_exact_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method, expected) in [
        ("survey", "ClaimSurveyRewards", Value::None),
        ("survey", "PerformSurveyChecks", Value::None),
        ("survey", "GetActiveSurveyID", Value::None),
        ("pvpFilamentMgr", "GetAllEvents", Value::Dict(vec![])),
        ("pvpFilamentMgr", "GetActiveEvents", Value::Dict(vec![])),
        ("pvpFilamentMgr", "GetMostRecentEvent", Value::None),
        ("pvpFilamentMgr", "GetNextEventDate", Value::None),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let value = root(&call).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        assert!(root(&Call { service: "other", ..call }).is_none());
        assert!(root(&Call { service: if service == "survey" { "pvpFilamentMgr" } else { "survey" }, ..call }).is_none());
    }
    for method in ["JoinPVPQueue", "LeavePVPQueue"] {
        let args = [Value::Int(1), Value::Int(1)];
        let call = Call { service: "pvpFilamentMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("expected refusal") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::pvp_filaments::UNAVAILABLE))]);
        assert!(root(&Call { service: "survey", ..call }).is_none());
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    for (service, method) in [("survey", "Unknown"), ("pvpFilamentMgr", "Unknown"), ("pvpFilamentMgr", "GetLeaderboard"), ("pvpFilamentMgr", "GetCharacterStatistics")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}
