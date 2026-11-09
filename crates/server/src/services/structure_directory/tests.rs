use super::*;

#[test]
fn directory_replies_have_client_shapes_and_exact_ownership() {
    let kwargs = Value::Dict(vec![]);
    for (method, expected) in [
        ("GetMyCharacterStructures", Value::Dict(vec![])),
        ("GetMyCorporationStructures", Value::Dict(vec![])),
        ("GetStructureMapData", Value::list([])),
        ("GetMyDockableStructures", Value::list([])),
        ("GetSolarSystemsWithBeacons", Value::list([])),
        ("GetNearbyJumpBridges", Value::list([])),
        ("GetJumpBridgesWithMyAccess", Value::tuple([Value::list([]), Value::list([]), Value::list([])])),
        ("GetStructureDescription", Value::unicode("")),
    ] {
        let call = Call { service: "structureDirectory", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&call).unwrap().unwrap();
        assert_eq!(reply, expected, "{method}");
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), expected);
        assert!(super::super::fixed::universe(&call).is_none());
        assert!(super::super::fixed::station(&call).is_none());
        assert!(super::super::fixed::root(&call).is_none());
    }
    let call = Call { service: "structureDirectory", method: "SetStructureDescription", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let super::super::CallError::User { msg, dict } = dispatch(&call).unwrap().unwrap_err() else { panic!("expected UserError") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::structures::UNAVAILABLE))]);
    let wire = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
    for (service, method) in [("otherService", "GetMyCharacterStructures"), ("structureDirectory", "UnknownMethod"), ("structureDirectory", "GetStructureInfo"), ("structureDirectory", "CheckMyDockingAccessToStructures"), ("structureDirectory", "GetMyAccessibleOnlineCynoBeaconStructures"), ("structureDirectory", "GetValidWarHQs")] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(dispatch(&call).is_none());
    }
}
