use super::*;

#[test]
fn seasonal_ownership_and_wire_shapes() {
    let kwargs = Value::Dict(vec![]);
    for (service, method, args, expected, phase) in [
        ("rewardMgr", "GetDelayedRewardsByGroupIDs", vec![Value::tuple([])], Value::Dict(vec![]), universe as fn(&Call<'_>) -> Option<CallResult>),
        ("seasonManager", "get_season_data_for_character", vec![], Value::None, station),
        ("battlepassSeasonMgr", "CallSeasonDataRequested", vec![], Value::None, station),
        ("milestoneMgr", "ProcessCharacterLogon", vec![], Value::None, station),
        ("activityMgr", "GetActivityBadgeStatuses", vec![], Value::list([]), station),
        ("activityMgr", "ActivitySeen", vec![Value::Int(1)], Value::None, station),
    ] {
        let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let value = phase(&call).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        assert!(phase(&Call { service: "other", ..call }).is_none());
    }
    for (service, method, args, text) in [
        ("seasonManager", "reload_fsd_data", vec![], infinity_core::messages::season::ACTION_UNAVAILABLE),
        ("bluebirdIndustryMgr", "RedeemTicket", vec![Value::Int(1)], infinity_core::messages::bluebird::TICKET_REDEMPTION_UNAVAILABLE),
    ] {
        let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("expected refusal") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    for (service, method, args, call_kwargs) in [
        ("seasonManager", "get_character_progress_by_character_id", vec![], kwargs.clone()),
        ("seasonManager", "select_season", vec![Value::Int(1)], kwargs.clone()),
        ("seasonManager", "get_challenge", vec![Value::Int(1)], kwargs.clone()),
        ("seasonManager", "get_max_active_challenges", vec![], kwargs.clone()),
        ("seasonManager", "challenge_is_dormant", vec![Value::Int(1)], kwargs.clone()),
        ("seasonManager", "get_challenge_expiration_date", vec![Value::Int(1)], kwargs.clone()),
        ("seasonManager", "get_season_end_time", vec![], kwargs.clone()),
        ("seasonManager", "get_loot_table_by_typeID", vec![Value::Int(1)], kwargs.clone()),
        ("seasonManager", "fill_up_and_return_challenges_for_character", vec![], kwargs.clone()),
        ("seasonManager", "claim_challenge_rewards", vec![Value::Int(1), Value::str("Challenge Card")], kwargs.clone()),
        ("seasonManager", "claim_goal_reward", vec![Value::Int(1)], kwargs.clone()),
        ("battlepassSeasonMgr", "CallCompleted", vec![Value::Int(1), Value::Int(1)], kwargs.clone()),
        ("battlepassSeasonMgr", "CallRewardClaimed", vec![Value::Int(1), Value::Int(1)], kwargs.clone()),
        ("battlepassSeasonMgr", "CallExtraGachaRewardClaimed", vec![Value::Int(1)], kwargs.clone()),
        ("battlepassSeasonMgr", "CallQAFinishMission", vec![Value::Int(1), Value::Int(1)], kwargs.clone()),
        ("milestoneMgr", "ClaimRewards", vec![Value::Int(1)], kwargs.clone()),
        ("seasonManager", "process_client_event", vec![Value::Int(1)], Value::Dict(vec![(Value::str("extra_data"), Value::Dict(vec![(Value::str("typeID"), Value::Int(1))])), (Value::str("amount"), Value::Int(1))])),
        ("seasonManager", "Unknown", vec![], kwargs.clone()),
        ("rewardMgr", "Unknown", vec![], kwargs.clone()),
        ("other", "GetDelayedRewardsByGroupIDs", vec![Value::tuple([])], kwargs.clone()),
    ] {
        let call = Call { service, method, args: &args, kwargs: &call_kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
        assert!(universe(&call).is_none());
        assert!(station(&call).is_none());
    }
}
