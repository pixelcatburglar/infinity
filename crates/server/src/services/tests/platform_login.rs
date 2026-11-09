use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn platform_login_dispatch_preserves_identity_and_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = client(test_db.clone(), owner.character).await;
        let (other, _other_peer) = super::client(test_db.clone(), foreign.character).await;
        client.login.user_id = owner.account;
        client.session.apply(vec![("userid", Value::Int(owner.account)), ("charid", Value::None)]);
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let pre_session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let bound = client.bound.clone();
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        let hash = [Value::str("fixture-hash")];
        let login = Call { service: "multiLoginBlocker", method: "Login", args: &hash, kwargs: &kwargs, bound: false, bind_params: None };
        let reply = dispatch(&mut client, &login).await.unwrap();
        assert_eq!(reply, Value::Bool(true));
        assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
        assert_eq!(client.login.user_id, owner.account);
        assert_eq!(client.session.int("charid"), None);
        assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), pre_session);
        assert_eq!(client.session.sid, sid);
        assert_eq!(client.bound, bound);
        assert_eq!(client.world.online_pilots(), pilots);
        assert!(client.outbox.is_empty());
        assert!(other.outbox.is_empty());
        assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
        assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);

        client.session.apply(vec![("charid", Value::Int(owner.character))]);
        let selected_session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let initial = Call { service: "onlineStatus", method: "GetInitialState", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let initial_reply = dispatch(&mut client, &initial).await.unwrap();
        let header = marshal::DbRowDescriptor::new([("contactID", marshal::DbType::I4), ("online", marshal::DbType::Bool)]);
        assert_eq!(initial_reply, crowset(&header, vec![]));
        assert_eq!(client.session.int("charid"), Some(owner.character));
        let selected_reply = dispatch(&mut client, &login).await.unwrap();
        assert_eq!(selected_reply, Value::Bool(true));
        assert_eq!(marshal::loads(&marshal::dumps(&selected_reply)).unwrap(), selected_reply);
        assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), selected_session);
        assert_eq!(dispatch(&mut client, &initial).await.unwrap(), initial_reply);
        assert_eq!(client.session.int("charid"), Some(owner.character));
        assert_eq!(client.login.user_id, owner.account);

        let s = Value::str;
        let cases = vec![
            ("kiringMgr", "ShouldKickCheater", vec![], Value::Bool(false)),
            ("kiringMgr", "RMTQuerySellOrdersByCharacterID", vec![Value::Int(owner.character)], Value::list([])),
            ("kiringMgr", "GetUnisdkPaymentConfig", vec![], Value::Dict(vec![])),
            ("eveguard_report", "verify_cloud_game_disable_key", vec![s("fixture-key")], Value::Bool(false)),
            ("eveguard_report", "heartbeat_started", vec![], Value::None),
            ("eveguard_report", "heartbeat_stopped", vec![], Value::None),
        ];
        for (service, method, args, expected) in cases {
            let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(value, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        }
        for (id, character) in [(own_item, owner.character), (foreign_item, foreign.character), (-1, -1)] {
            let refusals = vec![
                ("GetEveGuardServerAddress", vec![], infinity_core::messages::platform::GUARD_UNAVAILABLE),
                ("RMTSellIsk", vec![s("fixture-account"), Value::Int(1), Value::Int(1), Value::Int(1)], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTSignup", vec![s("fixture-account")], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTCancelSignup", vec![], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTSellCharacter", vec![s("fixture-account"), Value::Int(1), Value::Int(1), Value::Bool(false), Value::list([Value::Int(id)]), Value::list([Value::Int(id)])], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTRetrySaleOrder", vec![s("fixture-account"), Value::Int(id)], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTRetrySaleOrderWhenLogin", vec![Value::Int(character), s("fixture-account"), Value::Int(id)], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTTakeBackItem", vec![Value::Int(id)], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTTakeBackItemWhenLogin", vec![Value::Int(character), Value::Int(id)], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("RMTTakeAwayItem", vec![Value::Int(id)], infinity_core::messages::platform::RMT_UNAVAILABLE),
                ("PerformKiringServerSideAuthenticationFromCode", vec![s("fixture-code"), s("fixture-device"), s("fixture-client")], infinity_core::messages::platform::AUTH_UNAVAILABLE),
                ("GetSAuthAid", vec![Value::Int(owner.account), s("fixture-account"), s("fixture-device")], infinity_core::messages::platform::AUTH_UNAVAILABLE),
                ("PlaceKiringOrder", vec![s("fixture-device"), s("fixture-client"), s("fixture-aid"), s("fixture-goods"), Value::Int(1), Value::Int(owner.account), s("fixture-account"), s("fixture-url"), s("fixture-app"), s("fixture-pay"), s("fixture-login"), Value::Dict(vec![]), s("fixture-mode")], infinity_core::messages::platform::PAYMENT_UNAVAILABLE),
                ("ActivateRedeemingCode", vec![s("fixture-code"), s("fixture-account"), s("fixture-aid"), Value::Int(owner.account), s("fixture-name"), s("fixture-ip"), s("fixture-device")], infinity_core::messages::platform::PAYMENT_UNAVAILABLE),
            ];
            for (method, args, text) in refusals {
                let call = Call { service: "kiringMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {method}") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                assert!(client.outbox.is_empty());
                assert!(other.outbox.is_empty());
            }
        }
        assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
        assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
        assert_eq!(client.session.sid, sid);
        assert_eq!(client.login.user_id, owner.account);
        assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), selected_session);
        assert_eq!(client.bound, bound);
        assert_eq!(client.world.online_pilots(), pilots);
        assert!(client.outbox.is_empty());
        assert!(other.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
