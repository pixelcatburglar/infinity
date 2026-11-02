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
