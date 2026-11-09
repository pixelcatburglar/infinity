use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn travel_closed_dispatch_preserves_state() {
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
        for item in [own_item, foreign_item, -1] {
            for (service, method, args, text) in [
                ("voidSpaceMgr", "VoidSpacePlayerJump", vec![Value::Int(item), Value::Int(i64::from(infinity_core::consts::TYPE_PLEX))], infinity_core::messages::void_space::UNAVAILABLE),
                ("randomJumpMgr", "ActivateRandomJumpFilament", vec![Value::Int(item)], infinity_core::messages::random_jump::UNAVAILABLE),
                ("starterShipcasterTravelMgr", "CanCharacterJump", vec![], infinity_core::messages::shipcaster::UNAVAILABLE),
                ("amplificationMatrixSolarsystemMgr", "ActiveSkill", vec![Value::Int(1), Value::Int(item)], infinity_core::messages::AMPLIFICATION_MATRIX_UNAVAILABLE),
                ("amplificationMatrixSolarsystemMgr", "InactiveSkill", vec![Value::Int(1), Value::Int(item)], infinity_core::messages::AMPLIFICATION_MATRIX_UNAVAILABLE),
                ("amplificationMatrixSolarsystemMgr", "SyncMatrixDbuff", vec![], infinity_core::messages::AMPLIFICATION_MATRIX_UNAVAILABLE),
                ("amplificationMatrixSolarsystemMgr", "TransferCash", vec![Value::Int(item)], infinity_core::messages::AMPLIFICATION_MATRIX_UNAVAILABLE),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {service}.{method}") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                assert!(client.outbox.is_empty());
                assert!(other.outbox.is_empty());
            }
        }
        for (service, method, expected) in [
            ("voidSpaceMgr", "ClientIsReady", Value::None),
            ("solarsystemInterferenceMgr", "GetAllInterferenceBands", Value::Dict(vec![])),
            ("solarsystemInterferenceMgr", "GetLocalInterferenceState", Value::reduce("solarsysteminterference.solarsystemInterferenceState.SolarsystemInterferenceState", vec![Value::Float(0.0), Value::Int(0), Value::Float(1.0), Value::Int(1), Value::Float(1.0), Value::Float(0.0)], None)),
        ] {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(value, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), expected);
        }
        let packed = Value::dict([
            ("slot1MatrixID", Value::Int(0)), ("slot1MatrixTypeID", Value::Int(0)),
            ("slot1DurationEndTime", Value::Int(0)), ("slot1CdEndTime", Value::Int(0)),
            ("slot2MatrixID", Value::Int(0)), ("slot2MatrixTypeID", Value::Int(0)),
            ("slot2DurationEndTime", Value::Int(0)), ("slot2CdEndTime", Value::Int(0)),
        ]);
        for _ in 0..2 {
            let call = Call { service: "amplificationMatrixSolarsystemMgr", method: "FetchMatrixPacked", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
            assert_eq!(client.outbox.len(), 1);
            let packet = client.outbox.pop().unwrap();
            let packet = crate::macho::Packet::from_value(&marshal::loads(&marshal::dumps(&packet.to_value())).unwrap()).unwrap();
            assert_eq!(packet.command, crate::macho::cmd::NOTIFICATION);
            assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id == Value::str("OnMatrixPacked")));
            let envelope = packet.body[0].items().unwrap();
            let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
            assert_eq!(payload.items().unwrap()[1].items().unwrap(), &[packed.clone()]);
            assert!(other.outbox.is_empty());
        }
        let unknown = Call { service: "amplificationMatrixSolarsystemMgr", method: "Unknown", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(super::super::amplification_matrix::dispatch(&mut client, &unknown).is_none());
        assert_eq!(dispatch(&mut client, &unknown).await.unwrap(), Value::None);
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
