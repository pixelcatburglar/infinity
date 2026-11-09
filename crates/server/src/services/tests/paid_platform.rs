use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn paid_platform_dispatch_preserves_keyed_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, peer) = super::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let owner_item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, 1, 0, 1, true) RETURNING item_id", &[&owner.character]).await.unwrap().get("item_id");
        let foreign_item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, 1, 0, 1, true) RETURNING item_id", &[&foreign.character]).await.unwrap().get("item_id");
        test_db.execute("INSERT INTO game.redeem_tokens (user_id, type_id, quantity, created) VALUES ($1, 1, 1, 1), ($2, 1, 1, 1)", &[&owner.account, &foreign.account]).await.unwrap();
        let before_owner = snapshot(&test_db, owner).await;
        let before_foreign = snapshot(&test_db, foreign).await;
        let skills = crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap();
        let owner_queue = crate::dao::skills::queue::list(&test_db, owner.character).await.unwrap();
        let foreign_queue = crate::dao::skills::queue::list(&test_db, foreign.character).await.unwrap();
        let owner_rewards = crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap();
        let foreign_rewards = crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap();
        let kwargs = Value::Dict(vec![]);
        for docked in [true, false] {
            client.session.apply(vec![("stationid", if docked { Value::Int(60003760) } else { Value::None }), ("solarsystemid", if docked { Value::None } else { Value::Int(30000142) })]);
            let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            let sid = client.session.sid;
            let login = (client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone());
            let bound = client.bound.clone();
            let pilots = client.world.online_pilots();
            for _ in 0..2 {
                let call = Call { service: "nes_intro", method: "get_nes_intro_state", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
                assert!(fixed::universe(&call).is_some());
                let value = dispatch(&mut client, &call).await.unwrap();
                assert_eq!(value, Value::Int(0));
                assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
            }
            for (service, method, args, text) in [
                ("raffleMgr", "QA_SeedRaffles", vec![Value::Int(1)], infinity_core::messages::raffles::SEEDING_UNAVAILABLE),
                ("gachaMgr", "InjectTickets", vec![Value::Int(owner_item), Value::Int(1)], infinity_core::messages::gacha::TICKET_INJECTION_UNAVAILABLE),
                ("gachaMgr", "InjectTickets", vec![Value::Int(foreign_item), Value::Int(1)], infinity_core::messages::gacha::TICKET_INJECTION_UNAVAILABLE),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_some());
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
            }
            let ack = Call { service: "monetizationMgr", method: "CallUserRequested", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
            assert!(fixed::station(&ack).is_some());
            assert_eq!(dispatch(&mut client, &ack).await.unwrap(), Value::None);
            for (service, method, args) in [
                ("nes_intro", "skip_nes_intro", vec![]),
                ("contextualOfferMgr", "TrackOfferDelivered", vec![Value::Int(1)]),
                ("contextualOfferMgr", "TrackOfferSeen", vec![Value::Int(1)]),
                ("contextualOfferMgr", "TrackOfferBuyPress", vec![Value::Int(1)]),
                ("contextualOfferMgr", "TrackOfferClosed", vec![Value::Int(1)]),
                ("contextualOfferMgr", "TrackOfferPurchased", vec![Value::Int(1)]),
                ("contextualOfferMgr", "TrackOfferPurchasedSeen", vec![Value::Int(1)]),
                ("monetizationMgr", "CallOfferRequested", vec![]),
                ("monetizationMgr", "CallOfferClicked", vec![Value::Int(1)]),
                ("monetizationMgr", "CallRequested", vec![Value::Int(1)]),
                ("monetizationMgr", "CallActivityEntryRewardClaimed", vec![Value::Int(1), Value::Int(1)]),
                ("cloudComputingMgr", "CallCloudComputingOpened", vec![]),
                ("cloudComputingMgr", "CallAddShipClicked", vec![Value::Int(owner_item), Value::Int(1)]),
                ("cloudComputingMgr", "CallRemoveShipClicked", vec![Value::Int(owner_item)]),
                ("cloudComputingMgr", "CallRemoveAllShipsClicked", vec![]),
                ("cache", "Rowset", vec![Value::Int(2000209992)]),
                ("monetizationMgr", "Unknown", vec![]), ("other", "InjectTickets", vec![]),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_none() && fixed::universe(&call).is_none() && fixed::station(&call).is_none());
                assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::None);
            }
            assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
            assert_eq!(client.session.sid, sid);
            assert_eq!((client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone()), login);
            assert_eq!(client.bound, bound);
            assert_eq!(client.world.online_pilots(), pilots);
            assert!(client.outbox.is_empty());
            assert_eq!(snapshot(&test_db, owner).await, before_owner);
            assert_eq!(snapshot(&test_db, foreign).await, before_foreign);
            assert!(crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap() == skills);
            assert!(crate::dao::skills::queue::list(&test_db, owner.character).await.unwrap() == owner_queue);
            assert!(crate::dao::skills::queue::list(&test_db, foreign.character).await.unwrap() == foreign_queue);
            assert_eq!(crate::dao::tests::reward_snapshot(&test_db, owner.account).await.unwrap(), owner_rewards);
            assert_eq!(crate::dao::tests::reward_snapshot(&test_db, foreign.account).await.unwrap(), foreign_rewards);
            let mut bytes = [0u8; 1];
            assert!(matches!(peer.try_read(&mut bytes), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
        }
    }).await;
    db.execute("DELETE FROM game.redeem_tokens WHERE user_id = $1 OR user_id = $2", &[&owner.account, &foreign.account]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

async fn snapshot(db: &crate::db::Db, f: fixture::Fixture) -> (String, String, String) {
    let account: String = db.query_one("SELECT row_to_json(a)::text AS state FROM game.accounts a WHERE user_id = $1", &[&f.account]).await.unwrap().get("state");
    let rewards: String = db.query_one("SELECT COALESCE(string_agg(row_to_json(r)::text, '' ORDER BY token_id), '') AS state FROM game.redeem_tokens r WHERE user_id = $1", &[&f.account]).await.unwrap().get("state");
    (account, rewards, crate::dao::tests::structure_snapshot(db, f.character).await.unwrap())
}
