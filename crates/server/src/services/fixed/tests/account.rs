use super::*;

#[test]
fn account_exact_ownership_and_wire_replies() {
    let kwargs = Value::Dict(vec![]);
    for (service, method, expected) in [
        ("userSvc", "InvalidateMultiCharacterTrainingCache", Value::None),
        ("userSvc", "GetUpdatedMultiCharacterTraining", Value::list([])),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let value = root(&call).unwrap().unwrap();
        assert_eq!(value, expected);
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    for (service, method, text) in [
        ("userSvc", "ActivateRenameCard", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "ActivateCharacterReSculpt", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "ReverseRedeem", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "ReportISKSpammer", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "CastUpvote", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "CastDownvote", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "ReportBot", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "CreateRedeemTokenQA", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("communityTrackMgr", "change_qa_mode", infinity_core::messages::account::ACTION_UNAVAILABLE),
        ("userSvc", "ActivateMultiTraining", infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
        ("userSvc", "ActivateSoulboundMultiTraining", infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let Err(crate::services::CallError::User { msg, dict }) = root(&call).unwrap() else { panic!("{service}.{method}") };
        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
        let wire = crate::client::user_error(&msg, dict);
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        assert!(root(&Call { service: "other", ..call }).is_none());
    }
    for (service, method) in [
        ("characterEnergyMgr", "GetMyEnergyState"), ("userSvc", "GetUserToken"),
        ("userSvc", "GetUserName"), ("userSvc", "GetCreateDate"),
        ("communityTrackMgr", "get_instance_attributes_public"), ("userSvc", "Unknown"),
        ("infoGatheringMgr", "LogInfoEventsFromClient"),
        ("subscriptionMgr", "KickAlphaUsersOnTheSameComputer"),
        ("userSvc", "GetSessionPlaytimeMinutes"),
    ] {
        let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(root(&call).is_none());
    }
}
