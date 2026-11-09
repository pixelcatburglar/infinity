use super::*;

#[test]
fn platform_login_exact_replies_and_fallthrough() {
    let kwargs = Value::Dict(vec![]);
    for (service, method, expected) in [
        ("kiringMgr", "ShouldKickCheater", Value::Bool(false)),
        ("kiringMgr", "RMTQuerySellOrdersByCharacterID", Value::list([])),
        ("kiringMgr", "GetUnisdkPaymentConfig", Value::Dict(vec![])),
        ("eveguard_report", "verify_cloud_game_disable_key", Value::Bool(false)),
        ("eveguard_report", "heartbeat_started", Value::None),
        ("eveguard_report", "heartbeat_stopped", Value::None),
        ("multiLoginBlocker", "Login", Value::Bool(true)),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let value = root(&call).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    for (method, text) in [
        ("GetEveGuardServerAddress", infinity_core::messages::platform::GUARD_UNAVAILABLE),
        ("RMTSellIsk", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTSignup", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTCancelSignup", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTSellCharacter", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTRetrySaleOrder", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTRetrySaleOrderWhenLogin", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTTakeBackItem", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTTakeBackItemWhenLogin", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("RMTTakeAwayItem", infinity_core::messages::platform::RMT_UNAVAILABLE),
        ("PerformKiringServerSideAuthenticationFromCode", infinity_core::messages::platform::AUTH_UNAVAILABLE),
        ("GetSAuthAid", infinity_core::messages::platform::AUTH_UNAVAILABLE),
        ("PlaceKiringOrder", infinity_core::messages::platform::PAYMENT_UNAVAILABLE),
        ("ActivateRedeemingCode", infinity_core::messages::platform::PAYMENT_UNAVAILABLE),
    ] {
        let call = Call { service: "kiringMgr", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("expected refusal for {method}") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    for method in ["CanSellCharacterToSpecific", "GetZhStoreOfficialWebsiteUrl", "RMTQuerySellOrderList", "RMTQueryBuyOrderList", "RMTQueryHistoryOrderList", "RMTQuerySignupState", "RMTQuerySellISKLimitations", "RMTQuerySellCharacterLimitations", "RMTQueryISKRecommendPrice", "Unknown"] {
        let call = Call { service: "kiringMgr", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
    for service in ["eveguard_report", "multiLoginBlocker"] {
        let call = Call { service, method: "Unknown", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}
