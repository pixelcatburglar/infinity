use super::*;

const REPORTS: &[&str] = &[
    "report_inappropriate_language", "report_offensive_mail", "report_macro_use",
    "report_questionable_transaction", "report_inappropriate_character_name", "report_exploit_abuse",
    "report_gambling", "report_other", "report_protection_of_minors",
];
const REFUSALS: &[&str] = &[
    "MayPetition", "CreatePetition", "UpdatePetitionRating", "AddPetitionRating",
    "PetitioneeChat", "PetitionerChat", "DeletePetition", "CancelPetition", "ClosePetition",
    "UnClaimPetition", "EscalatePetition", "ClaimPetition", "GetZendeskJwtLink",
];
const UNHANDLED: &[&str] = &[
    "GetUserCatalogCountry", "GetCategoryProperties", "PropertyPopulationInfo", "GetClientPickerInfo",
    "GetClaimedPetitions", "GetPetitionQueue", "GetQueues", "GetCategories", "GetLog",
];

#[test]
fn support_exact_ownership_and_wire_replies() {
    let kwargs = Value::Dict(vec![]);
    for (service, methods, text) in [
        ("ingamereport", REPORTS, infinity_core::messages::support::REPORT_UNAVAILABLE),
        ("petitioner", REFUSALS, infinity_core::messages::support::PETITION_UNAVAILABLE),
    ] {
        for &method in methods {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            assert!(crate::services::lua_fixed_claimed(&call));
            let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("{service}.{method}") };
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
            let wire = crate::client::user_error(&msg, dict);
            assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
            let wrong = Call { service: "other", ..call };
            assert!(root(&wrong).is_none());
            assert!(!crate::services::lua_fixed_claimed(&wrong));
        }
    }
    for (method, expected) in [
        ("GetCategoryHierarchicalInfo", Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![])])),
        ("GetMyPetitionsEx", Value::list([])), ("GetPetitionMessages", Value::list([])),
        ("MarkAsRead", Value::None), ("HasOpenTickets", Value::Bool(false)), ("IsSerenity", Value::Bool(true)),
    ] {
        let call = Call { service: "petitioner", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(crate::services::lua_fixed_claimed(&call));
        let value = root(&call).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
        let wrong = Call { service: "other", ..call };
        assert!(root(&wrong).is_none());
        assert!(!crate::services::lua_fixed_claimed(&wrong));
    }
    for &method in UNHANDLED.iter().chain(["Unknown", "CreatePetitions"].iter()) {
        let call = Call { service: "petitioner", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
        assert!(!crate::services::lua_fixed_claimed(&call));
    }
    let unknown = Call { service: "ingamereport", method: "report_unknown", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(root(&unknown).is_none());
    assert!(!crate::services::lua_fixed_claimed(&unknown));
    for (method, expected) in [("IsZendeskEnabled", Value::Bool(false)), ("IsZendeskSwapEnabled", Value::Bool(false)), ("GetUnreadMessages", Value::list([]))] {
        let call = Call { service: "petitioner", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
        assert!(!crate::services::lua_fixed_claimed(&call));
        assert_eq!(station(&call).unwrap().unwrap(), expected);
    }
    let call = Call { service: "charUnboundMgr", method: "GetCharacterInfo", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert!(crate::services::lua_fixed_claimed(&call));
    assert_eq!(root(&call).unwrap().unwrap(), Value::list([]));
}
