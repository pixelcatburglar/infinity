//! Real slash dispatch for the personal and corporation EverMarks commands and
//! the organization-lock membership recheck of `gm_change`.

use super::*;
use crate::services::tests::fixture;
use crate::services::slash::tests::support;
use infinity_core::consts::{CORP_HERALDRY, GM_ROLES};
use infinity_core::session::OnlineSession;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn evermarks_commands_use_exact_current_owners() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let gm_fx = fixture::create(&db).await.unwrap();
    let member_fx = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, gm_fx.character, 0).await.unwrap();
    fixture::assign_corporation(&db, gm_fx.character, corp, 0).await.unwrap();
    fixture::assign_corporation(&db, member_fx.character, corp, 0).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let mut npc = sde.npc_corporations.iter();
    let decoy = npc.next().expect("first SDE NPC corporation").corporation_id;
    let npc_corp = npc.next().expect("second SDE NPC corporation").corporation_id;
    let npc_before = crate::dao::wallets::loyalty::balance(&db, npc_corp, CORP_HERALDRY).await.unwrap();
    let test_db = db.clone();
    let result = crate::tests::catch_scenario(async {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), gm_fx.character).await;
        client.login.role = GM_ROLES;
        // The session deliberately disagrees: ownership always comes from the database.
        client.session.apply(vec![("corpid", Value::Int(decoy))]);
        let decoy_before = crate::dao::wallets::loyalty::balance(&test_db, decoy, CORP_HERALDRY).await.unwrap();
        let mut gm_rx = support::register_online(&client.world, OnlineSession::from_session(&client.session).unwrap());
        let mut member_rx = support::register_online(&client.world, OnlineSession { character_id: member_fx.character, user_id: None, session_id: 0, ship_id: None, station_id: None, system_id: None, corporation_id: Some(corp), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 });
        let exact = 9_007_199_254_740_993_i64;
        let reply = support::rpc(&mut client, "/evermarks").await.unwrap();
        assert_eq!(reply.as_text().unwrap(), messages::gm::gm_evermarks_balance(gm_fx.character, false, 0));
        support::rpc(&mut client, "/setevermarks 9007199254740993").await.unwrap();
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, gm_fx.character, CORP_HERALDRY).await.unwrap(), Some(exact));
        assert_eq!(gm_rx.try_recv().unwrap(), ("OnLPChange".to_string(), vec![Value::str("lp"), Value::Int(CORP_HERALDRY), Value::Int(0), Value::Int(exact)]));
        support::rpc(&mut client, "/addevermarks -4").await.unwrap();
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, gm_fx.character, CORP_HERALDRY).await.unwrap(), Some(exact - 4));
        assert_eq!(gm_rx.try_recv().unwrap().1, vec![Value::str("lp"), Value::Int(CORP_HERALDRY), Value::Int(exact), Value::Int(exact - 4)]);
        support::rpc(&mut client, "/addevermarks 0").await.unwrap();
        assert!(gm_rx.try_recv().is_err());
        support::rpc(&mut client, "/addcorpevermarks 5").await.unwrap();
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, corp, CORP_HERALDRY).await.unwrap(), Some(5));
        for rx in [&mut gm_rx, &mut member_rx] {
            assert_eq!(rx.try_recv().unwrap(), ("OnCorpLPChange".to_string(), vec![Value::str("lp"), Value::Int(CORP_HERALDRY)]));
        }
        let reply = support::rpc(&mut client, "/corpevermarks").await.unwrap();
        assert_eq!(reply.as_text().unwrap(), messages::gm::gm_evermarks_balance(corp, true, 5));
        support::rpc(&mut client, "/setcorpevermarks 9007199254740993").await.unwrap();
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, corp, CORP_HERALDRY).await.unwrap(), Some(exact));
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, decoy, CORP_HERALDRY).await.unwrap(), decoy_before);
        for rx in [&mut gm_rx, &mut member_rx] {
            assert_eq!(rx.try_recv().unwrap(), ("OnCorpLPChange".to_string(), vec![Value::str("lp"), Value::Int(CORP_HERALDRY)]));
        }
        let (mut member_client, _member_peer) = crate::services::tests::client(test_db.clone(), member_fx.character).await;
        assert!(support::rpc(&mut member_client, "/evermarks").await.is_err());
        member_client.login.role = GM_ROLES;
        fixture::assign_corporation(&test_db, member_fx.character, npc_corp, 0).await.unwrap();
        let mut npc_rx = support::register_online(&member_client.world, OnlineSession { character_id: member_fx.character, user_id: None, session_id: 0, ship_id: None, station_id: None, system_id: None, corporation_id: Some(npc_corp), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 });
        support::rpc(&mut member_client, "/addcorpevermarks 3").await.unwrap();
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, npc_corp, CORP_HERALDRY).await.unwrap(), Some(npc_before.unwrap_or(0) + 3));
        assert_eq!(npc_rx.try_recv().unwrap(), ("OnCorpLPChange".to_string(), vec![Value::str("lp"), Value::Int(CORP_HERALDRY)]));
        assert!(member_rx.try_recv().is_err());
        for line in ["/setevermarks -1", "/addevermarks 1.5", "/addevermarks 9223372036854775808", "/addevermarks 1 2", "/evermarks 1", "/setcorpevermarks 1 2"] {
            assert!(support::rpc(&mut client, line).await.is_err(), "{line} must be refused");
        }
        assert!(crate::db::outbox::tests::events(&test_db, "LoyaltyPointsEarned", gm_fx.character).await.is_empty());
        assert!(crate::db::outbox::tests::events(&test_db, "EverMarksEarned", gm_fx.character).await.is_empty());
        client.world.unregister_pilot_generation(gm_fx.character, None);
        client.world.unregister_pilot_generation(member_fx.character, None);
        member_client.world.unregister_pilot_generation(member_fx.character, None);
    }).await;
    let owners = vec![gm_fx.character, member_fx.character, corp, npc_corp];
    db.execute("DELETE FROM game.lp_balances WHERE corp_id = $1 AND char_id = ANY($2)", &[&CORP_HERALDRY, &owners]).await.unwrap();
    if let Some(saved) = npc_before {
        db.execute("INSERT INTO game.lp_balances (char_id, corp_id, amount) VALUES ($1, $2, $3)", &[&npc_corp, &CORP_HERALDRY, &saved]).await.unwrap();
    }
    fixture::cleanup_corporation(&db, corp, &[gm_fx.character, member_fx.character], 0).await.unwrap();
    fixture::cleanup(&db, gm_fx).await.unwrap();
    fixture::cleanup(&db, member_fx).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn corporation_change_rechecks_membership_after_lock_wait() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let ceo_b = fixture::create(&db).await.unwrap();
    let corp_a = fixture::create_corporation(&db, f.character, 0).await.unwrap();
    let corp_b = fixture::create_corporation(&db, ceo_b.character, 0).await.unwrap();
    fixture::assign_corporation(&db, f.character, corp_a, 0).await.unwrap();
    let test_db = db.clone();
    let result = crate::tests::catch_scenario(async {
        let (client, _peer) = crate::services::tests::client(test_db.clone(), f.character).await;
        let (mut pg, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let holder = pg.transaction().await.unwrap();
        assert!(registry::lock_admission(&holder, &[], &[corp_a], &[f.character]).await.unwrap());
        let mut single_config = config.clone();
        single_config.pool_size = 1;
        let single = crate::db::Db::connect(&single_config).await.unwrap();
        let waiter_pid: i32 = single.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        let sde = client.sde.clone();
        let world = client.world.clone();
        let identity = Identity { char_id: f.character, ship_id: None, corp_id: corp_a, station_id: None, system_id: None };
        let change = tokio::spawn(async move { gm_change(&single, &world, &sde, identity, CORP_HERALDRY, Scope::Corporation, Change::Add(1)).await });
        let mut waited = false;
        for _ in 0..200 {
            let blocked: bool = test_db.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND cardinality(pg_blocking_pids(pid)) > 0) AS blocked",
                &[&waiter_pid],
            ).await.unwrap().get("blocked");
            if blocked { waited = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(waited, "gm_change did not wait on the organization locks");
        holder.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&f.character, &corp_b]).await.unwrap();
        holder.commit().await.unwrap();
        match change.await.unwrap() {
            Err(CallError::User { dict, .. }) => assert!(dict.iter().any(|(key, value)| key == "notify" && value.as_text().is_some_and(|text| text.as_ref() == messages::gm::GM_LP_MEMBERSHIP_CHANGED))),
            _ => panic!("membership change must refuse the corporation write"),
        }
        for corp in [corp_a, corp_b] {
            assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, corp, CORP_HERALDRY).await.unwrap(), None);
        }
        test_db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&f.character, &corp_a]).await.unwrap();
        test_db.execute("UPDATE game.corporations SET deleted = true WHERE corporation_id = $1", &[&corp_a]).await.unwrap();
        assert!(gm_change(&test_db, &client.world, &client.sde, identity, CORP_HERALDRY, Scope::Corporation, Change::Add(1)).await.is_err());
    }).await;
    db.execute("DELETE FROM game.lp_balances WHERE corp_id = $1 AND char_id = ANY($2)", &[&CORP_HERALDRY, &vec![f.character, corp_a, corp_b]]).await.unwrap();
    fixture::cleanup_corporation(&db, corp_a, &[f.character], 0).await.unwrap();
    fixture::cleanup_corporation(&db, corp_b, &[f.character], 0).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    fixture::cleanup(&db, ceo_b).await.unwrap();
    result.unwrap();
}
