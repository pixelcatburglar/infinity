//! CEO candidate blocking is authoritative even for the stored CEO transition.
use infinity_core::consts::ROLE_DIRECTOR;
use crate::{db::Db, services::{corps::roles, tests::{self, fixture}}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn blocked_member_cannot_become_ceo() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let candidate = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, candidate.character];
    db.execute("UPDATE game.characters SET corporation_id=$1, station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_block_roles=true WHERE character_id=$1", &[&candidate.character]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        assert!(roles::resign_ceo(&test_db, &client.sde, &client.world, ceo.character, corp, Some(candidate.character)).await.is_err());
        assert_eq!(crate::dao::corporations::registry::ceo_of(&test_db, corp).await.unwrap(), Some(ceo.character));
        assert!(crate::dao::corporations::roles::potential_ceos(&test_db, corp, ceo.character).await.unwrap().is_empty());
    }).await;
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_block_roles=false WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, candidate).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn nonempty_dissolution_keeps_corporation_and_assets() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    let ids = [ceo.character, member.character];
    db.execute("UPDATE game.characters SET corporation_id=$1,station_id=60000001 WHERE character_id=ANY($2)", &[&corp, &ids.as_slice()]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
    for id in ids { db.execute("INSERT INTO game.employment_history(char_id,corporation_id,start_date) VALUES ($1,$2,1)", &[&id, &corp]).await.unwrap(); }
    db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,1000,17)", &[&corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        test_db.execute("UPDATE game.characters SET station_id=NULL WHERE character_id=$1", &[&ceo.character]).await.unwrap();
        assert!(roles::resign_ceo(&test_db, &client.sde, &client.world, ceo.character, corp, None).await.is_err());
        assert!(!crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap().deleted);
        test_db.execute("UPDATE game.characters SET station_id=60000001 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
        roles::resign_ceo(&test_db, &client.sde, &client.world, ceo.character, corp, None).await.unwrap();
        let row = crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap();
        assert!(row.deleted);
        assert_eq!(row.member_count, 0);
        let remaining: bool = test_db.query_one("SELECT EXISTS(SELECT 1 FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=1000 AND balance=17) AS present", &[&corp]).await.unwrap().get("present");
        assert!(remaining);
        assert!(test_db.query("SELECT receiver_id FROM game.notifications WHERE type_id=85 AND receiver_id=ANY($1) AND sender_id=$2", &[&ids.as_slice(), &corp]).await.unwrap().len() >= 2);
        let notices = test_db.query("SELECT data FROM game.notifications WHERE type_id=85 AND receiver_id=ANY($1) AND sender_id=$2", &[&ids.as_slice(), &corp]).await.unwrap();
        assert!(notices.iter().all(|row| row.get::<_, String>("data").contains(&corp.to_string())));
    }).await;
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.employment_history WHERE char_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=ANY($1)", &[&ids.as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, member).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_open_orders_block_final_dissolution_but_personal_orders_do_not() {
    use infinity_core::consts::ACCOUNT_CASH;
    use crate::services::{identity::Identity, market::{BuyRequest, lifecycle::cancel_order, trading::place_buy}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let (mut client, _peer) = tests::client(db.clone(), ceo.character).await;
    let at = fixture::isolated_market_stations(&db, &client.sde).await.unwrap().next().expect("isolated NPC station");
    let (station, system, region) = (at.id, at.system, at.region);
    let ty = fixture::market_type_ids(&client.sde).next().expect("tradable market SDE type");
    let corp = fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2,balance=10000000 WHERE character_id=$1", &[&ceo.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,$2,10000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &(ACCOUNT_CASH + 1)]).await.unwrap();
    client.session.apply(vec![("corpid", marshal::Value::Int(corp)), ("stationid", marshal::Value::Int(station)), ("solarsystemid2", marshal::Value::Int(system))]);
    let scenario = tokio::spawn(async move {
        let identity = Identity::of_client(&client);
        let request = BuyRequest { station_id: station, type_id: ty.into(), price: 0.01, quantity: 1, range: -1, min_volume: 1, duration: 1, expected_brokers_fee: None };
        let order = place_buy(&identity, &test_db, &client.world, &client.sde, request, Some(ACCOUNT_CASH + 1)).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(ceo.character, tx);
        let economy = fixture::market_fingerprint(&test_db, &[corp], &[ceo.character]).await.unwrap();
        let corporation: String = test_db.query_one("SELECT row_to_json(t)::text AS state FROM game.corporations t WHERE corporation_id=$1", &[&corp]).await.unwrap().get("state");
        let governance: String = test_db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(t)::text,'' ORDER BY row_to_json(t)::text),'') FROM game.corp_member_events t WHERE corporation_id=$1) || (SELECT COALESCE(string_agg(row_to_json(t)::text,'' ORDER BY row_to_json(t)::text),'') FROM game.notifications t WHERE sender_id=$1 OR receiver_id=$2) || (SELECT COALESCE(string_agg(row_to_json(t)::text,'' ORDER BY row_to_json(t)::text),'') FROM game.employment_history t WHERE char_id=$2) AS state", &[&corp, &ceo.character]).await.unwrap().get("state");
        let before: String = test_db.query_one("SELECT row_to_json(o)::text AS row FROM game.market_orders o WHERE order_id=$1", &[&order.id]).await.unwrap().get("row");
        let effects = test_db.query_one("SELECT (SELECT count(*) FROM game.corp_member_events WHERE corporation_id=$1) AS audit, (SELECT count(*) FROM game.notifications WHERE sender_id=$1) AS notices, (SELECT count(*) FROM game.employment_history WHERE char_id=$2) AS employment", &[&corp, &ceo.character]).await.unwrap();
        let effects = (effects.get::<_, i64>("audit"), effects.get::<_, i64>("notices"), effects.get::<_, i64>("employment"));
        let result = roles::resign_ceo(&test_db, &client.sde, &client.world, ceo.character, corp, None).await;
        assert!(matches!(result, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), marshal::Value::unicode(infinity_core::messages::market::CORPORATION_OPEN_ORDERS))]));
        assert_eq!(fixture::market_fingerprint(&test_db, &[corp], &[ceo.character]).await.unwrap(), economy);
        assert_eq!(test_db.query_one("SELECT row_to_json(t)::text AS state FROM game.corporations t WHERE corporation_id=$1", &[&corp]).await.unwrap().get::<_, String>("state"), corporation);
        let after_governance: String = test_db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(t)::text,'' ORDER BY row_to_json(t)::text),'') FROM game.corp_member_events t WHERE corporation_id=$1) || (SELECT COALESCE(string_agg(row_to_json(t)::text,'' ORDER BY row_to_json(t)::text),'') FROM game.notifications t WHERE sender_id=$1 OR receiver_id=$2) || (SELECT COALESCE(string_agg(row_to_json(t)::text,'' ORDER BY row_to_json(t)::text),'') FROM game.employment_history t WHERE char_id=$2) AS state", &[&corp, &ceo.character]).await.unwrap().get("state");
        assert_eq!(after_governance, governance);
        assert!(rx.try_recv().is_err());
        assert!(client.outbox.is_empty());
        assert!(!crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap().deleted);
        assert_eq!(test_db.query_one("SELECT row_to_json(o)::text AS row FROM game.market_orders o WHERE order_id=$1", &[&order.id]).await.unwrap().get::<_, String>("row"), before);
        let after = test_db.query_one("SELECT (SELECT count(*) FROM game.corp_member_events WHERE corporation_id=$1) AS audit, (SELECT count(*) FROM game.notifications WHERE sender_id=$1) AS notices, (SELECT count(*) FROM game.employment_history WHERE char_id=$2) AS employment", &[&corp, &ceo.character]).await.unwrap();
        assert_eq!((after.get::<_, i64>("audit"), after.get::<_, i64>("notices"), after.get::<_, i64>("employment")), effects);
        cancel_order(&identity, &test_db, &client.world, &client.sde, order.id, region).await.unwrap();
        let personal = place_buy(&identity, &test_db, &client.world, &client.sde, request, None).await.unwrap();
        assert_eq!(roles::resign_ceo(&test_db, &client.sde, &client.world, ceo.character, corp, None).await.unwrap(), marshal::Value::None);
        assert!(crate::dao::corporations::registry::corporation(&test_db, corp).await.unwrap().unwrap().deleted);
        assert_eq!(test_db.query("SELECT event_id FROM game.corp_member_events WHERE corporation_id=$1 AND character_id=$2 AND issuer_id=$2 AND event_type_id=$3", &[&corp, &ceo.character, &super::super::audit::DISSOLVED]).await.unwrap().len(), 1);
        let notices = test_db.query("SELECT data FROM game.notifications WHERE sender_id=$1 AND receiver_id=$2 AND type_id=85", &[&corp, &ceo.character]).await.unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&notices[0].get::<_, String>("data")).unwrap(), serde_json::json!({"corpID": corp}));
        assert!(test_db.query_opt("SELECT order_id FROM game.market_orders WHERE order_id=$1", &[&personal.id]).await.unwrap().is_some());
    }).await;
    let owners = vec![ceo.character, corp];
    db.transaction(|tx, _: &mut ()| Box::pin(async move { fixture::cleanup_progress(tx, &[corp], &[corp]).await }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE char_id=$1 OR corporation_id=$2", &[&ceo.character, &corp]).await.unwrap();
    db.execute("DELETE FROM game.market_order_history WHERE char_id=$1 OR corporation_id=$2", &[&ceo.character, &corp]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE sender_id=$1 AND receiver_id=$2", &[&corp, &ceo.character]).await.unwrap();
    db.execute("DELETE FROM game.employment_history WHERE char_id=$1", &[&ceo.character]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    scenario.unwrap();
}
