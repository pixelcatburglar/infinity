use super::*;

#[test]
fn paid_platform_ownership_and_wire_shapes() {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "nes_intro", method: "get_nes_intro_state", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let value = universe(&call).unwrap().unwrap();
    assert_eq!(value, Value::Int(0));
    assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
    for (service, method, args, text) in [
        ("raffleMgr", "QA_SeedRaffles", vec![Value::Int(1)], infinity_core::messages::raffles::SEEDING_UNAVAILABLE),
        ("gachaMgr", "InjectTickets", vec![Value::Int(1), Value::Int(1)], infinity_core::messages::gacha::TICKET_INJECTION_UNAVAILABLE),
    ] {
        let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("expected refusal") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    let call = Call { service: "monetizationMgr", method: "CallUserRequested", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(station(&call).unwrap().unwrap(), Value::None);
    for (service, method) in [
        ("nes_intro", "skip_nes_intro"),
        ("contextualOfferMgr", "TrackOfferDelivered"), ("contextualOfferMgr", "TrackOfferSeen"),
        ("contextualOfferMgr", "TrackOfferBuyPress"), ("contextualOfferMgr", "TrackOfferClosed"),
        ("contextualOfferMgr", "TrackOfferPurchased"), ("contextualOfferMgr", "TrackOfferPurchasedSeen"),
        ("monetizationMgr", "CallOfferRequested"), ("monetizationMgr", "CallOfferClicked"),
        ("monetizationMgr", "CallRequested"), ("monetizationMgr", "CallActivityEntryRewardClaimed"),
        ("cloudComputingMgr", "CallCloudComputingOpened"), ("cloudComputingMgr", "CallAddShipClicked"),
        ("cloudComputingMgr", "CallRemoveShipClicked"), ("cloudComputingMgr", "CallRemoveAllShipsClicked"),
        ("cache", "Rowset"), ("monetizationMgr", "Unknown"), ("other", "get_nes_intro_state"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none() && universe(&call).is_none() && station(&call).is_none(), "{service}.{method}");
    }
}

#[test]
fn paid_platform_gates_are_not_advertised() {
    let config = crate::config::Config::default().global_config();
    for key in [
        "ship_cloud_computing_enabled", "gacha_enabled", "recruitment_enabled", "mobilecollect_enabled",
        "monetization_recommended_enabled", "monetization_cashback_enabled",
        "monetization_plex_spend_reward_enabled", "monetization_accumulative_purchase_enabled",
        "monetization_daily_purchase_reward_enabled",
    ] {
        assert!(config.get(key).is_none(), "{key}");
    }
}
