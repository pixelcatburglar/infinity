use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn keeper_boundary_dispatch_preserves_fixture_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = client(test_db.clone(), owner.character).await;
        let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
        let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
        let skills_owner = fixture::skill_fingerprint(&test_db, owner.character).await.unwrap();
        let skills_foreign = fixture::skill_fingerprint(&test_db, foreign.character).await.unwrap();
        let rewards_owner = crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap();
        let rewards_foreign = crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap();
        let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
        let sid = client.session.sid;
        let bound = client.bound.clone();
        let outbox = client.outbox.len();
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        let system = Value::Int(1);
        for (character, item) in [(owner.character, own_item), (foreign.character, foreign_item)] {
            let cases = vec![
                ("keeper", "GetOperationSpawnpointSolarSystem", vec![Value::Int(character), Value::Int(item)], Some(Value::None), ""),
                ("keeper", "IsOperationSite", vec![Value::Int(item)], Some(Value::Bool(false)), ""),
                ("keeper", "GetCurrentDungeonForCharacter", vec![Value::Int(character)], Some(Value::None), ""),
                ("keeper", "GetLevelEditor", vec![], None, infinity_core::messages::npe::LEVEL_EDITOR_UNAVAILABLE),
                ("beyonce", "GetOperationSpawnpointPosition", vec![Value::Int(item)], None, infinity_core::messages::npe::OPERATION_SITE_UNAVAILABLE),
            ];
            for (service, method, args, expected, message) in cases {
                let is_bound = service == "beyonce";
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: is_bound, bind_params: if is_bound { Some(&system) } else { None } };
                match (dispatch(&mut client, &call).await, expected) {
                    (Ok(value), Some(expected)) => {
                        assert_eq!(value, expected, "{service}.{method}");
                        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
                    }
                    (Err(CallError::User { msg, dict }), None) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(message))]);
                        let wire = crate::client::user_error(&msg, dict);
                        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                    }
                    (other, _) => panic!("unexpected result for {service}.{method}: {other:?}"),
                }
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
                assert_eq!(fixture::skill_fingerprint(&test_db, owner.character).await.unwrap(), skills_owner);
                assert_eq!(fixture::skill_fingerprint(&test_db, foreign.character).await.unwrap(), skills_foreign);
                assert_eq!(crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap(), rewards_owner);
                assert_eq!(crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap(), rewards_foreign);
                assert_eq!(client.session.sid, sid);
                assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                assert_eq!(client.bound, bound);
                assert_eq!(client.outbox.len(), outbox);
                assert_eq!(client.world.online_pilots(), pilots);
                let mut byte = [0];
                assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            }
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
