use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn world_catalogue_dispatch_preserves_state_and_queues_local_responses() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (other, other_peer) = client(test_db.clone(), foreign.character).await;
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
        let pilots = client.world.online_pilots();
        let kwargs = Value::Dict(vec![]);
        for (service, method) in [("other", "RequestAllTask"), ("worldTaskMgr", "Unknown")] {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            assert!(super::super::world_tasks::dispatch(&mut client, &call).is_none());
        }
        for (service, method) in [("taleMgr", "Unknown"), ("custom_agency_provider", "Unknown"), ("worldTaskContentMgr", "Unknown"), ("other", "warp_to_content")] {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            assert!(super::super::fixed::root(&call).is_none());
            assert!(super::super::fixed::universe(&call).is_none());
        }
        for item in [own_item, foreign_item] {
            let cases = vec![
                ("worldTaskMgr", "ActiveTask", vec![Value::Int(1)], false),
                ("worldTaskMgr", "AbortedTask", vec![Value::Int(1)], false),
                ("worldTaskMgr", "Search", vec![Value::Int(1), Value::Int(1)], false),
                ("worldTaskMgr", "Select", vec![Value::Int(1)], false),
                ("worldTaskMgr", "SubmitItems", vec![Value::Int(1), Value::list([Value::tuple([Value::Int(item), Value::Int(i64::from(infinity_core::consts::TYPE_PLEX)), Value::Int(1)])])], false),
                ("worldTaskMgr", "Complete", vec![Value::Int(1)], false),
                ("worldTaskContentMgr", "GetDungeonShipRestrictions", vec![Value::Int(1)], false),
                ("custom_agency_provider", "warp_to_content", vec![Value::Int(1)], false),
                ("taleMgr", "get_active_tales_by_template", vec![Value::Int(1)], true),
                ("taleMgr", "GetGlobalWorldEventTales", vec![], true),
                ("custom_agency_provider", "get_content_data", vec![], true),
            ];
            for (service, method, args, empty) in cases {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                if empty {
                    let value = dispatch(&mut client, &call).await.unwrap();
                    assert_eq!(value, Value::list([]));
                    assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
                } else {
                    let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {service}.{method}") };
                    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                    assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::world_tasks::UNAVAILABLE))]);
                    let wire = crate::client::user_error(&msg, dict);
                    assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                }
                assert!(client.outbox.is_empty());
            }
        }
        for _ in 0..2 {
            for (method, event, text) in [
                ("RequestAllTask", "OnWorldTaskResponded", ""),
                ("RequestUserData", "OnWorldTaskUserDataResponded", "rest_scan_count: 0\nmax_scan_count: 0\nnext_scan_count_add_time { seconds: 0 nanos: 0 }\nunlock_fight_mission_level: 0\nunlock_support_mission_level: 0\n"),
            ] {
                let call = Call { service: "worldTaskMgr", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
                let value = dispatch(&mut client, &call).await.unwrap();
                assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
                assert_eq!(client.outbox.len(), 1);
                let packet = client.outbox.pop().unwrap();
                let packet = crate::macho::Packet::from_value(&marshal::loads(&marshal::dumps(&packet.to_value())).unwrap()).unwrap();
                assert_eq!(packet.command, crate::macho::cmd::NOTIFICATION);
                assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id == Value::str(event)));
                let envelope = packet.body[0].items().unwrap();
                let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
                assert_eq!(payload, Value::tuple([Value::Int(1), Value::tuple([Value::str(text)])]));
                assert!(other.outbox.is_empty());
            }
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
        assert_eq!(client.world.online_pilots(), pilots);
        for peer in [&peer, &other_peer] {
            let mut byte = [0];
            assert_eq!(peer.try_read(&mut byte).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
