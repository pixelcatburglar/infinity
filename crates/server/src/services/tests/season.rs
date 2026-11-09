use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn seasonal_dispatch_preserves_keyed_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = super::client(test_db.clone(), owner.character).await;
        client.login.user_id = owner.account;
        let owner_item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, 1, 0, 1, true) RETURNING item_id", &[&owner.character]).await.unwrap().get("item_id");
        let foreign_item: i64 = test_db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, $1, 1, 0, 1, true) RETURNING item_id", &[&foreign.character]).await.unwrap().get("item_id");
        test_db.execute("INSERT INTO game.redeem_tokens (user_id, type_id, quantity, created) VALUES ($1, 1, 1, 1), ($2, 1, 1, 1)", &[&owner.account, &foreign.account]).await.unwrap();
        let before_owner = snapshot(&test_db, owner).await;
        let before_foreign = snapshot(&test_db, foreign).await;
        let kwargs = Value::Dict(vec![]);
        for docked in [true, false] {
            client.session.apply(vec![("stationid", if docked { Value::Int(60003760) } else { Value::None }), ("solarsystemid", if docked { Value::None } else { Value::Int(30000142) })]);
            let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            let sid = client.session.sid;
            let login = (client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone());
            let bound = client.bound.clone();
            let pilots = client.world.online_pilots();
            for (service, method, args, expected) in [
                ("rewardMgr", "GetDelayedRewardsByGroupIDs", vec![Value::tuple([])], Value::Dict(vec![])),
                ("rewardMgr", "GetDelayedRewardsByGroupIDs", vec![Value::tuple([Value::Int(1), Value::Int(2)])], Value::Dict(vec![])),
                ("seasonManager", "get_season_data_for_character", vec![], Value::None),
                ("battlepassSeasonMgr", "CallSeasonDataRequested", vec![], Value::None),
                ("milestoneMgr", "ProcessCharacterLogon", vec![], Value::None),
                ("activityMgr", "GetActivityBadgeStatuses", vec![], Value::list([])),
                ("activityMgr", "ActivitySeen", vec![Value::Int(1)], Value::None),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert!(fixed::universe(&call).is_some() || fixed::station(&call).is_some());
                let value = dispatch(&mut client, &call).await.unwrap();
                assert_eq!(value, expected);
                assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
            }
            for (service, method, args, text) in [
                ("seasonManager", "reload_fsd_data", vec![], infinity_core::messages::season::ACTION_UNAVAILABLE),
                ("bluebirdIndustryMgr", "RedeemTicket", vec![Value::Int(owner_item)], infinity_core::messages::bluebird::TICKET_REDEMPTION_UNAVAILABLE),
                ("bluebirdIndustryMgr", "RedeemTicket", vec![Value::Int(foreign_item)], infinity_core::messages::bluebird::TICKET_REDEMPTION_UNAVAILABLE),
            ] {
                let call = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_some());
                let Err(CallError::User { msg, dict }) = dispatch(&mut client, &call).await else { panic!("expected refusal") };
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
                let wire = crate::client::user_error(&msg, dict);
                assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
            }
            for (service, method, args, call_kwargs) in [
                ("seasonManager", "get_character_progress_by_character_id", vec![], kwargs.clone()),
                ("seasonManager", "select_season", vec![Value::Int(1)], kwargs.clone()),
                ("seasonManager", "get_challenge", vec![Value::Int(1)], kwargs.clone()),
                ("seasonManager", "get_max_active_challenges", vec![], kwargs.clone()),
                ("seasonManager", "challenge_is_dormant", vec![Value::Int(1)], kwargs.clone()),
                ("seasonManager", "get_challenge_expiration_date", vec![Value::Int(1)], kwargs.clone()),
                ("seasonManager", "get_season_end_time", vec![], kwargs.clone()),
                ("seasonManager", "get_loot_table_by_typeID", vec![Value::Int(1)], kwargs.clone()),
                ("seasonManager", "fill_up_and_return_challenges_for_character", vec![], kwargs.clone()),
                ("seasonManager", "claim_challenge_rewards", vec![Value::Int(1), Value::str("Challenge Card")], kwargs.clone()),
                ("seasonManager", "claim_goal_reward", vec![Value::Int(1)], kwargs.clone()),
                ("battlepassSeasonMgr", "CallCompleted", vec![Value::Int(1), Value::Int(1)], kwargs.clone()),
                ("battlepassSeasonMgr", "CallRewardClaimed", vec![Value::Int(1), Value::Int(1)], kwargs.clone()),
                ("battlepassSeasonMgr", "CallExtraGachaRewardClaimed", vec![Value::Int(1)], kwargs.clone()),
                ("battlepassSeasonMgr", "CallQAFinishMission", vec![Value::Int(1), Value::Int(1)], kwargs.clone()),
                ("milestoneMgr", "ClaimRewards", vec![Value::Int(1)], kwargs.clone()),
                ("seasonManager", "process_client_event", vec![Value::Int(1)], Value::Dict(vec![(Value::str("extra_data"), Value::Dict(vec![(Value::str("typeID"), Value::Int(1))])), (Value::str("amount"), Value::Int(1))])),
                ("seasonManager", "Unknown", vec![], kwargs.clone()),
                ("rewardMgr", "Unknown", vec![], kwargs.clone()),
                ("other", "GetDelayedRewardsByGroupIDs", vec![Value::tuple([])], kwargs.clone()),
            ] {
                let call = Call { service, method, args: &args, kwargs: &call_kwargs, bound: false, bind_params: None };
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
