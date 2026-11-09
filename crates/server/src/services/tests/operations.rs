use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn operations_dispatch_preserves_fixture_state() {
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
        let outbox = client.outbox.len();
        let pilots = client.world.online_pilots();
        let empty = Value::Dict(vec![]);
        // Initialization reads progress then the active category. With None, the
        // client does not request start_site or unblock its first active task.
        for (method, expected) in [("get_character_progress", Value::Dict(vec![])), ("get_active_category_id", Value::None), ("can_character_play_the_tutorial", Value::Bool(false))] {
            let args = if method == "can_character_play_the_tutorial" { vec![Value::Int(owner.character)] } else { vec![] };
            let call = Call { service: "operationsManager", method, args: &args, kwargs: &empty, bound: false, bind_params: None };
            assert_eq!(dispatch(&mut client, &call).await.unwrap(), expected);
            assert_eq!(crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap(), rewards_owner);
            assert_eq!(crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap(), rewards_foreign);
        }
        let air = Call { service: "air_npe", method: "is_air_npe_enabled", args: &[], kwargs: &empty, bound: false, bind_params: None };
        assert_eq!(dispatch(&mut client, &air).await.unwrap(), Value::Bool(false));
        assert_eq!(crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap(), rewards_owner);
        assert_eq!(crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap(), rewards_foreign);
        let air_state = Call { method: "get_air_npe_state", ..air };
        assert_eq!(dispatch(&mut client, &air_state).await.unwrap(), Value::Int(3));
        assert_eq!(crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap(), rewards_owner);
        assert_eq!(crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap(), rewards_foreign);
        for (character, item) in [(owner.character, own_item), (foreign.character, foreign_item)] {
            let c = Value::Int(character);
            let i = Value::Int(item);
            let cases = vec![
                ("get_character_progress", vec![], Some(Value::Dict(vec![]))),
                ("get_active_category_id", vec![], Some(Value::None)),
                ("get_operations_completed_at_least_once", vec![c.clone()], Some(Value::list([]))),
                ("get_mission_avoidance_systems", vec![], Some(Value::list([]))),
                ("skip_task", vec![c.clone(), i.clone(), Value::Int(1)], Some(Value::Bool(false))),
                ("activate_operation_in_solar_system", vec![i.clone(), Value::Int(1)], Some(Value::Bool(false))),
                ("is_category_complete", vec![i.clone()], Some(Value::Bool(false))),
                ("get_tutorial_state", vec![], None),
                ("is_main_tutorial_finished", vec![c.clone()], None),
                ("has_skipped_tutorial", vec![], None),
                ("start_site", vec![i.clone(), Value::Int(1)], None),
                ("complete_category_for_character", vec![c.clone(), i.clone()], None),
                ("block_task", vec![c.clone(), i.clone(), Value::Int(1)], None),
                ("unblock_task", vec![c.clone(), i.clone(), Value::Int(1)], None),
                ("get_category_state", vec![i.clone()], None),
                ("process_client_event", vec![i.clone()], None),
            ];
            for (method, args, expected) in cases {
                let event_kwargs = Value::dict([("extra_data", Value::dict([("character_id", c.clone()), ("item_id", i.clone())])), ("amount", Value::Int(1))]);
                let kwargs = if method == "process_client_event" { &event_kwargs } else { &empty };
                let call = Call { service: "operationsManager", method, args: &args, kwargs, bound: false, bind_params: None };
                match (dispatch(&mut client, &call).await, expected) {
                    (Ok(value), Some(expected)) => {
                        assert_eq!(value, expected, "{method}");
                        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
                    }
                    (Err(CallError::User { msg, dict }), None) => {
                        assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                        assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::npe::OPERATIONS_UNAVAILABLE))]);
                        let wire = crate::client::user_error(&msg, dict);
                        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                    }
                    (other, _) => panic!("unexpected result for {method}: {other:?}"),
                }
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
                assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
                assert_eq!(client.session.sid, sid);
                assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
                assert_eq!(client.outbox.len(), outbox);
                assert_eq!(client.world.online_pilots(), pilots);
                assert_eq!(fixture::skill_fingerprint(&test_db, owner.character).await.unwrap(), skills_owner);
                assert_eq!(fixture::skill_fingerprint(&test_db, foreign.character).await.unwrap(), skills_foreign);
                assert_eq!(crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap(), rewards_owner);
                assert_eq!(crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap(), rewards_foreign);
                assert!(client.world.ball(1, item).await.is_none());
                let mut byte = [0];
                assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
            }
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
