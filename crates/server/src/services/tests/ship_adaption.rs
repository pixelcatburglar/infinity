use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn adaptation_dispatch_is_local_and_preserves_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (other, _other_peer) = client(test_db.clone(), foreign.character).await;
        let (mut client, _peer) = client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
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
        for (service, method) in [("other", "fetch_tree_data"), ("shipAdaptionMgr", "Unknown")] {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            assert!(super::super::ship_adaption::dispatch(&mut client, &call).await.is_none());
        }
        let chips = super::super::keyval(vec![("type_id", Value::Int(i64::from(infinity_core::consts::TYPE_PLEX))), ("quantity", Value::Int(1))]);
        let mut cases = vec![
            ("call_finish_node_mission", vec![Value::Int(1)]),
            ("call_assign_mission", vec![Value::Int(1)]),
            ("call_chip_exchange", vec![Value::Int(1), Value::list([chips])]),
        ];
        for method in ["call_finish_node_mission", "InjectChips", "call_assign_mission", "call_unlock_node", "call_chip_exchange"] {
            cases.push((method, vec![]));
        }
        for item in [own_item, foreign_item] {
            let offered = super::super::keyval(vec![("item_id", Value::Int(item)), ("item_type", Value::Int(i64::from(infinity_core::consts::TYPE_PLEX))), ("quantity", Value::Int(1))]);
            cases.push(("InjectChips", vec![Value::Int(item), Value::Int(1)]));
            cases.push(("call_unlock_node", vec![Value::Int(1), Value::list([offered])]));
        }
        for (method, args) in cases {
            let call = Call { service: "shipAdaptionMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {method}") };
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::ship_adaption::UNAVAILABLE))]);
            let wire = crate::client::user_error(&msg, dict);
            assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
            assert!(client.outbox.is_empty());
            assert!(other.outbox.is_empty());
        }
        let args = [Value::Int(1)];
        let tree = Call { service: "shipAdaptionMgr", method: "fetch_tree_data", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(matches!(dispatch(&mut client, &tree).await, Err(CallError::User { .. })));
        for (method, event) in [("fetch_missions", "OnMissionResponsed"), ("fetch_user_data", "OnUserResponsed")] {
            let call = Call { service: "shipAdaptionMgr", method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
            assert_eq!(client.outbox.len(), 1);
            let packet = client.outbox.pop().unwrap();
            let packet = crate::macho::Packet::from_value(&marshal::loads(&marshal::dumps(&packet.to_value())).unwrap()).unwrap();
            assert_eq!(packet.command, crate::macho::cmd::NOTIFICATION);
            assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id == Value::str(event)));
            let envelope = packet.body[0].items().unwrap();
            let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
            let text = payload.items().unwrap()[1].items().unwrap()[0].as_bytes().unwrap();
            let name = if method == "fetch_missions" { "eve.shipadaption.mission.Responsed" } else { "eve.shipadaption.user.Responsed" };
            let descriptor = crate::gateway::message::pool().get_message_by_name(name).unwrap();
            let message = prost_reflect::DynamicMessage::parse_text_format(descriptor, std::str::from_utf8(text).unwrap()).unwrap();
            for (field, expected) in [("user", owner.account), ("character", owner.character)] {
                let value = message.get_field_by_name(field).unwrap();
                let prost_reflect::Value::Message(identifier) = value.as_ref() else { panic!("identifier must be a message") };
                let sequential = identifier.get_field_by_name("sequential").unwrap();
                let actual = match sequential.as_ref() { prost_reflect::Value::I32(id) => i64::from(*id), prost_reflect::Value::U32(id) => i64::from(*id), _ => panic!("unexpected identifier") };
                assert_eq!(actual, expected);
            }
            let field = if method == "fetch_missions" { "missions" } else { "chips" };
            assert!(matches!(message.get_field_by_name(field).unwrap().as_ref(), prost_reflect::Value::List(rows) if rows.is_empty()));
            if method == "fetch_missions" { assert!(!message.has_field_by_name("active_mission")); }
            assert!(other.outbox.is_empty());
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
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
