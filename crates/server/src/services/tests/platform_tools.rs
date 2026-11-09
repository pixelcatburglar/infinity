use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn platform_tools_dispatch_preserves_identity_and_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = client(test_db.clone(), owner.character).await;
        let (other, _other_peer) = super::client(test_db.clone(), foreign.character).await;
        client.login.user_id = owner.account;
        client.session.apply(vec![("userid", Value::Int(owner.account)), ("charid", Value::None)]);
        let login = (client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone());
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let skills = crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap();
        let owner_queue = crate::dao::skills::queue::list(&test_db, owner.character).await.unwrap();
        let foreign_queue = crate::dao::skills::queue::list(&test_db, foreign.character).await.unwrap();
        let sid = client.session.sid;
        let bound = client.bound.clone();
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        for selected in [false, true] {
            if selected { client.session.apply(vec![("charid", Value::Int(owner.character))]); }
            let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            for (service, method, args) in [
                ("faceverifyMgr", "FaceVerifySucceed", vec![Value::str("fixture-aeid"), Value::str("fixture-reference"), Value::str("fixture-ticket")]),
                ("webviewAuthService", "GetWebviewForwardToken", vec![Value::str("ceve-mahjong-server")]),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {service}.{method}") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::platform::AUTH_UNAVAILABLE))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                let wrong = Call { service: "other", ..call };
                assert_eq!(dispatch(&mut client, &wrong).await.unwrap(), Value::None);
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
                assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::None, "{service}.{method}");
            }
            assert_eq!((client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone()), login);
            assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
            assert_eq!(client.session.sid, sid);
            assert_eq!(client.bound, bound);
            assert_eq!(client.world.online_pilots(), pilots);
            assert!(client.outbox.is_empty());
            assert!(other.outbox.is_empty());
            assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
            assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
            assert!(crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap() == skills);
            assert!(crate::dao::skills::queue::list(&test_db, owner.character).await.unwrap() == owner_queue);
            assert!(crate::dao::skills::queue::list(&test_db, foreign.character).await.unwrap() == foreign_queue);
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
