use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn brawl_closed_dispatch_preserves_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, owner.character).await.unwrap();
    fixture::seed_skill(&db, foreign.character).await.unwrap();
    let own_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, owner.character, owner.character, 0, 1).await.unwrap();
    let foreign_item = crate::dao::items::transfers::insert(&db, infinity_core::consts::TYPE_PLEX, foreign.character, foreign.character, 0, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = super::client(test_db.clone(), owner.character).await;
        let (other, _other_peer) = super::client(test_db.clone(), foreign.character).await;
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
        for item in [own_item, foreign_item] {
            for (service, method, args) in [
                ("brawlMgr", "CancelMatch", vec![]),
                ("brawlMgr", "LaunchMatch", vec![Value::Int(0), Value::Int(1)]),
                ("brawlMgr", "LaunchMatch", vec![Value::Int(1), Value::None]),
                ("brawlMgr", "LeaveFight", vec![]),
                ("brawlMgr", "ChooseSpawnDungeon", vec![Value::Int(1), Value::Int(1), Value::Int(1)]),
                ("brawlMgr", "launch_match", vec![Value::Int(1)]),
                ("brawlMgr", "StartNewPlayerFight", vec![]),
                ("brawlNewPlayerMgr", "EnableSkillUpgrade", vec![]),
                ("brawlNewPlayerMgr", "Step3", vec![]),
                ("brawlNewPlayerMgr", "ChangeZoneStatusToPreset", vec![]),
                ("brawlNewPlayerMgr", "ActiveGate", vec![Value::Int(item)]),
                ("brawlNewPlayerMgr", "GenerateCloud", vec![]),
                ("brawlNewPlayerMgr", "CollectCloud", vec![Value::Int(item)]),
                ("brawlNewPlayerMgr", "ReleaseCloud", vec![]),
                ("brawlNewPlayerMgr", "SpawnFakePlayer", vec![]),
                ("brawlNewPlayerMgr", "UnlimitedEntropy", vec![]),
                ("brawlNewPlayerMgr", "ActiveEntropyModule", vec![Value::Int(1)]),
                ("brawlNewPlayerMgr", "Leave", vec![]),
                ("brawlNewPlayerMgr", "Step2", vec![]),
                ("brawlNewPlayerMgr", "Step1", vec![]),
                ("brawlGameplayMgr", "DropCloud", vec![Value::Int(1)]),
                ("brawlGameplayMgr", "AutoEquip", vec![]),
                ("brawlGameplayMgr", "ActiveEntropyModule", vec![Value::Int(1)]),
                ("brawlGameplayMgr", "UpgradeEntropyModule", vec![Value::Int(1)]),
                ("brawlGameplayMgr", "UnlockEntropyModule", vec![Value::Int(1)]),
                ("brawlGameplayMgr", "ReleaseCloud", vec![Value::Int(1)]),
                ("brawlGameplayMgr", "CollectCloud", vec![Value::Int(item)]),
                ("brawlGameplayMgr", "TakeKillingMedal", vec![Value::Int(item)]),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal for {service}.{method}") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::brawl::UNAVAILABLE))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
                assert!(client.outbox.is_empty());
                assert!(other.outbox.is_empty());
            }
        }
        for content in [Value::None, Value::Int(1), Value::None, Value::Int(1)] {
            let args = [content];
            let call = Call { service: "brawlGameplayMgr", method: "FetchCharacterData", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let value = dispatch(&mut client, &call).await.unwrap();
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
            assert_eq!(client.outbox.len(), 1);
            let packet = client.outbox.pop().unwrap();
            let packet = crate::macho::Packet::from_value(&marshal::loads(&marshal::dumps(&packet.to_value())).unwrap()).unwrap();
            assert_eq!(packet.command, crate::macho::cmd::NOTIFICATION);
            assert!(matches!(packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id == Value::str("OnServerBrawlStatusNotified")));
            let envelope = packet.body[0].items().unwrap();
            let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
            assert_eq!(payload.items().unwrap()[1].items().unwrap(), &[Value::Int(1)]);
            assert!(other.outbox.is_empty());
        }
        for (service, method) in [("brawlMgr", "Unknown"), ("brawlMgr", "cancelMatch"), ("brawlGameplayMgr", "fetchCharacterData"), ("brawlNewPlayerMgr", "step1"), ("BrawlMgr", "CancelMatch")] {
            let call = Call { service, method, args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            assert!(super::super::brawl::dispatch(&mut client, &call).is_none());
            assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::None);
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
        assert!(client.world.ball(1, own_item).await.is_none());
        assert!(client.world.ball(1, foreign_item).await.is_none());
        assert!(client.outbox.is_empty());
        assert!(other.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
