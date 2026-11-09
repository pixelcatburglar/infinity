use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn closed_survey_pvp_dispatch_preserves_state() {
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
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let bound = client.bound.clone();
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        for (service, method, expected) in [
            ("survey", "ClaimSurveyRewards", Value::None),
            ("survey", "PerformSurveyChecks", Value::None),
            ("survey", "GetActiveSurveyID", Value::None),
            ("pvpFilamentMgr", "GetAllEvents", Value::Dict(vec![])),
            ("pvpFilamentMgr", "GetActiveEvents", Value::Dict(vec![])),
            ("pvpFilamentMgr", "GetMostRecentEvent", Value::None),
            ("pvpFilamentMgr", "GetNextEventDate", Value::None),
        ] {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(value, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
            assert!(client.outbox.is_empty());
            assert!(other.outbox.is_empty());
        }
        for item in [own_item, foreign_item, -1] {
            for method in ["JoinPVPQueue", "LeavePVPQueue"] {
                let args = [Value::Int(item), Value::Int(i64::from(infinity_core::consts::TYPE_PLEX))];
                let call = Call { service: "pvpFilamentMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {method}") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::pvp_filaments::UNAVAILABLE))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                assert!(client.outbox.is_empty());
                assert!(other.outbox.is_empty());
            }
        }
        assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
        assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
        assert_eq!(client.session.sid, sid);
        assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
        assert_eq!(client.bound, bound);
        assert_eq!(client.world.online_pilots(), pilots);
        assert!(client.world.ball(1, own_item).await.is_none());
        assert!(client.world.ball(1, foreign_item).await.is_none());
        assert!(client.outbox.is_empty());
        assert!(other.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
