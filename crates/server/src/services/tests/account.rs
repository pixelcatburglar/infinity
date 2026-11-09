use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn account_boundaries_dispatch_preserves_fixture_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, owner.character).await.unwrap();
    fixture::seed_skill(&db, foreign.character).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = super::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let owner_item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, 1, 0, 1, true) RETURNING item_id", &[&owner.character]).await.unwrap().get("item_id");
        let foreign_item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, 1, 0, 1, true) RETURNING item_id", &[&foreign.character]).await.unwrap().get("item_id");
        client.session.apply(vec![("userid", Value::Int(owner.account))]);
        let login = (client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone());
        let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let bound = client.bound.clone();
        let pilots = client.world.online_pilots();
        let before_owner = snapshot(&test_db, owner).await;
        let before_foreign = snapshot(&test_db, foreign).await;
        let training = crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap();
        let empty = Value::Dict(vec![]);
        for (service, method, args, expected) in [
            ("userSvc", "InvalidateMultiCharacterTrainingCache", vec![Value::Int(owner.account)], Value::None),
            ("userSvc", "GetUpdatedMultiCharacterTraining", vec![Value::Bool(true)], Value::list([])),
        ] {
            let call = Call { service, method, args: &args, kwargs: &empty, bound: false, bind_params: None };
            assert!(fixed::root(&call).is_some());
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(value, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
        }
        let id = Value::Int(owner_item);
        let other = Value::Int(foreign.character);
        let other_item = Value::Int(foreign_item);
        let text = Value::unicode("fixture");
        let training_kwargs = Value::dict([("trainingID", Value::Int(1))]);
        let new_training_kwargs = Value::dict([("trainingID", Value::None)]);
        let soulbound_kwargs = Value::dict([("trainingID", Value::Int(1)), ("useHomeStation", Value::Bool(true))]);
        let qa_kwargs = Value::dict([
            ("userID", Value::Int(owner.account)), ("typeID", Value::Int(1)), ("quantity", Value::Int(1)),
            ("blueprintRuns", Value::Int(0)), ("blueprintMaterialLevel", Value::Int(0)),
            ("blueprintProductivityLevel", Value::Int(0)), ("expiry", Value::None),
            ("label", text.clone()), ("description", text.clone()), ("addedByContext", Value::Int(1)),
            ("addedByExtra", Value::None), ("available", Value::Bool(true)), ("soulbound", Value::Bool(true)),
        ]);
        for (service, method, args, kwargs, message) in [
            ("userSvc", "ActivateRenameCard", vec![id.clone(), text.clone()], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "ActivateCharacterReSculpt", vec![id.clone()], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "ReverseRedeem", vec![other_item.clone()], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "ReportISKSpammer", vec![other.clone(), Value::Int(1), Value::list([])], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "CastUpvote", vec![], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "CastDownvote", vec![], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "CastDownvote", vec![text.clone()], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "ReportBot", vec![other.clone()], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "CreateRedeemTokenQA", vec![], &qa_kwargs, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("communityTrackMgr", "change_qa_mode", vec![Value::Bool(true)], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("communityTrackMgr", "change_qa_mode", vec![Value::Bool(false)], &empty, infinity_core::messages::account::ACTION_UNAVAILABLE),
            ("userSvc", "ActivateMultiTraining", vec![id.clone()], &training_kwargs, infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
            ("userSvc", "ActivateMultiTraining", vec![id.clone()], &new_training_kwargs, infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
            ("userSvc", "ActivateMultiTraining", vec![other_item.clone()], &training_kwargs, infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
            ("userSvc", "ActivateSoulboundMultiTraining", vec![Value::Int(owner.character), Value::list([Value::tuple([Value::Int(1), Value::Int(1)])])], &soulbound_kwargs, infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
            ("userSvc", "ActivateSoulboundMultiTraining", vec![other.clone(), Value::list([Value::tuple([Value::Int(1), Value::Int(1)])])], &soulbound_kwargs, infinity_core::messages::skills::MULTI_CHARACTER_TRAINING_UNSUPPORTED),
        ] {
            let call = Call { service, method, args: &args, kwargs, bound: false, bind_params: None };
            assert!(fixed::root(&call).is_some());
            let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {service}.{method}") };
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(message))]);
            let wire = crate::client::user_error(&msg, dict);
            assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        }
        for (service, method, args) in [
            ("characterEnergyMgr", "GetMyEnergyState", vec![]),
            ("userSvc", "GetUserToken", vec![]),
            ("userSvc", "GetUserName", vec![Value::Int(owner.account)]),
            ("userSvc", "GetCreateDate", vec![]),
            ("communityTrackMgr", "get_instance_attributes_public", vec![Value::Int(1)]),
            ("infoGatheringMgr", "LogInfoEventsFromClient", vec![Value::Dict(vec![])]),
            ("subscriptionMgr", "KickAlphaUsersOnTheSameComputer", vec![]),
            ("userSvc", "GetSessionPlaytimeMinutes", vec![]),
            ("other", "ActivateRenameCard", vec![id.clone(), text.clone()]),
            ("userSvc", "Unknown", vec![]),
        ] {
            let call = Call { service, method, args: &args, kwargs: &empty, bound: false, bind_params: None };
            assert!(fixed::root(&call).is_none());
            assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::None);
        }
        assert_eq!((client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone()), login);
        assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
        assert_eq!(client.session.sid, sid);
        assert_eq!(client.bound, bound);
        assert_eq!(client.world.online_pilots(), pilots);
        assert!(client.outbox.is_empty());
        assert_eq!(snapshot(&test_db, owner).await, before_owner);
        assert_eq!(snapshot(&test_db, foreign).await, before_foreign);
        assert!(crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap() == training);
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

async fn snapshot(db: &crate::db::Db, f: fixture::Fixture) -> (String, String, String) {
    let account: String = db.query_one("SELECT (SELECT row_to_json(a)::text FROM game.accounts a WHERE user_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(w)::text, '' ORDER BY account_id), '') FROM game.wallet_accounts w WHERE owner_id = $1) || '|' || (SELECT COALESCE(string_agg(row_to_json(p)::text, '' ORDER BY row_to_json(p)::text), '') FROM game.store_purchases p WHERE user_id = $1) AS state", &[&f.account]).await.unwrap().get("state");
    (account, crate::dao::tests::structure_snapshot(db, f.character).await.unwrap(), fixture::skill_fingerprint(db, f.character).await.unwrap())
}
