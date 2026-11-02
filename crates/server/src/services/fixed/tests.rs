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
