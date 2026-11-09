use super::*;

#[test]
fn empty_catalogue_roundtrips_and_closes_selection_gates() {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "tourneyMgr", method: "GetTourneySetup", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let value = root(&call).unwrap().unwrap();
    let decoded = marshal::loads(&marshal::dumps(&value)).unwrap();
    assert_eq!(decoded, value);
    let Value::Tuple(parts) = decoded else { panic!("expected setup tuple") };
    let [Value::Dict(tournaments), Value::Dict(systems), Value::List(active)] = parts.as_slice() else { panic!("expected dictionaries and list") };
    assert!(tournaments.is_empty());
    assert!(systems.is_empty());
    assert!(active.is_empty());
}

#[test]
fn gated_and_unknown_calls_are_not_owned_by_any_fixed_stage() {
    let kwargs = Value::Dict(vec![]);
    for (service, method) in [
        ("tourneyMgr", "QueryShipList"),
        ("tourneyMgr", "GetPotentialMatches"),
        ("tourneyMgr", "GetAllCurrentSeriesMatchDetails"),
        ("tourneyMgr", "QueryTournamentDetails"),
        ("tourneyMgr", "GetTourneySetUp"),
        ("tourneyMgr", "Unknown"),
        ("other", "GetTourneySetup"),
    ] {
        let args = [Value::Int(1)];
        let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none() && universe(&call).is_none() && station(&call).is_none()
            && social_early(&call).is_none() && social(&call).is_none() && fleets(&call).is_none(), "{service}.{method}");
    }
}
