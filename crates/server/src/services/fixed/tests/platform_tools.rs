use super::*;

#[test]
fn platform_tools_exact_ownership_and_browser_replies() {
    let kwargs = Value::Dict(vec![]);
    for (service, method, args) in [
        ("faceverifyMgr", "FaceVerifySucceed", vec![Value::str("fixture-aeid"), Value::str("fixture-reference"), Value::str("fixture-ticket")]),
        ("webviewAuthService", "GetWebviewForwardToken", vec![Value::str("ceve-mahjong-server")]),
    ] {
        let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(crate::services::lua_fixed_claimed(&call));
        let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("expected refusal for {service}.{method}") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::platform::AUTH_UNAVAILABLE))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        let wrong = Call { service: "other", ..call };
        assert!(root(&wrong).is_none());
        assert!(!crate::services::lua_fixed_claimed(&wrong));
    }
    for (service, method) in [
        ("browserLockdownSvc", "GetDefaultHomePage"), ("browserLockdownSvc", "IsBrowserInLockdown"),
        ("eveacsMgr", "RequestHttpMethod"), ("eveacsMgr", "UploadReport"),
        ("authentication", "AmUnderage"), ("authentication", "AccruedTime"),
        ("publicQaToolsServer", "SlashCmd"), ("publicQaToolsServer", "CanGiveItemForMultifit"),
        ("machoNet", "SetGlobalConfigValue"), ("machoNet", "ReloadClientCodeHash"),
        ("localizationServer", "ReloadFSDPickle"),
        ("marsRPCMgr", "test1"), ("marsRPCMgr", "test2"), ("marsRPCMgr", "test7"),
        ("marsRPCMgr", "test9"), ("marsRPCMgr", "testSpawnNPC"),
        ("serviceGatewayMgr", "qa_is_hijack_mode_enabled"), ("serviceGatewayMgr", "qa_disable_hijack_mode"),
        ("serviceGatewayMgr", "qa_enable_hijack_mode"),
        ("envsdkGateway", "CheckMessageValid"), ("envsdkGateway", "CheckNameValidEx"),
        ("envsdkGateway", "CheckSearchValid"), ("cache", "Rowset"),
        ("faceverifyMgr", "Unknown"), ("webviewAuthService", "Unknown"),
        ("faceverifyMgr", "FaceVerifySuccess"), ("webviewAuthService", "GetWebviewForwardTokens"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none(), "{service}.{method}");
        assert!(!crate::services::lua_fixed_claimed(&call), "{service}.{method}");
    }
    for (method, expected) in [
        ("GetFlaggedSitesHash", Value::str("d751713988987e9331980363e24189ce")),
        ("GetFlaggedSitesList", Value::list([])),
    ] {
        let call = Call { service: "browserLockdownSvc", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(universe(&call).unwrap().unwrap(), expected);
    }
}
