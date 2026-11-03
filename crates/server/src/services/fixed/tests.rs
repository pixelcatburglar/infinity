use super::*;

#[test]
fn character_selection_fixed_answers_do_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "GetQAStarterSystemIDs"),
        ("otherService", "GetCharacterInfo"),
        ("charUnboundMgr", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn corporate_mining_ledger_fixed_answers_do_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "GetObserversWithMiningEvents"),
        ("otherService", "GetObserverLedger"),
        ("corpMiningLedger", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn mobile_binding_callback_does_not_match_other_calls() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("otherService", "MobileBindSucceed"),
        ("mobileCollectMgr", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn qa_mission_selection_matches_only_its_exact_call() {
    let kwargs = Value::None;
    for (service, method) in [("otherService", "GetPlausibleMissionIDs"), ("agentMgr", "OtherMethod")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn alpha_and_encoded_item_answers_match_only_exact_calls() {
    let kwargs = Value::None;
    for (service, method) in [
        ("otherService", "GetNextAvailableInjection"),
        ("alphaInjectorMgr", "OtherMethod"),
        ("otherService", "DecodeItem"),
        ("encodedItems", "OtherMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}

#[test]
fn character_structure_directory_is_an_empty_dict() {
    let kwargs = Value::Dict(vec![]);
    let call = Call {
        service: "structureDirectory",
        method: "GetMyCharacterStructures",
        args: &[],
        kwargs: &kwargs,
        bound: false,
        bind_params: None,
    };
    let reply = universe(&call).expect("directory call is handled").expect("directory call succeeds");
    assert_eq!(reply, Value::Dict(vec![]));
    assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::Dict(vec![]));

    for (service, method) in [
        ("otherService", "GetMyCharacterStructures"),
        ("structureDirectory", "UnknownMethod"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(universe(&call).is_none());
    }
}
