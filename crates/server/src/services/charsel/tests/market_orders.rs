//! Character deletion and real market-order obligations.
use super::*;
use crate::services::{identity::Identity, market::{BuyRequest, SellRequest, lifecycle::cancel_order, trading::{place_buy, place_sell}}, tests::{client, fixture}};

async fn selection_rpc(c: &mut Client, character: i64, method: &str) -> CallResult {
    let args = [Value::Int(character)];
    let kwargs = Value::None;
    let call = Call { service: "charUnboundMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(c, &call).await
}

async fn delete(c: &mut Client, character: i64) -> CallResult {
    selection_rpc(c, character, "DeleteCharacter").await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn personal_open_orders_block_deletion_without_confusing_corporate_issuance() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let pilot = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let (mut market, _a) = client(db.clone(), pilot.character).await;
    let at = fixture::isolated_market_stations(&db, &market.sde).await.unwrap().next().expect("isolated NPC station");
    let (station, system, region) = (at.id, at.system, at.region);
    let ty = fixture::market_type_ids(&market.sde).next().expect("tradable market SDE type");
    let corp = fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    fixture::assign_corporation(&db, pilot.character, corp, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.characters SET balance=10000000, station_id=$2, delete_prepare_at=1 WHERE character_id=$1", &[&pilot.character, &station]).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,$2,10000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&corp, &infinity_core::consts::ACCOUNT_CASH]).await.unwrap();
    market.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    let scenario = tokio::spawn(async move {
        let (mut selection, _b) = client(test_db.clone(), 0).await;
        selection.login.user_id = pilot.account;
        assert!(matches!(delete(&mut selection, ceo.character).await, Err(CallError::User { msg, .. }) if msg == messages::MSG_CHARACTER_NOT_FOUND));
        selection_rpc(&mut selection, pilot.character, "CancelCharacterDeletePrepare").await.unwrap();
        assert_eq!(delete(&mut selection, pilot.character).await.unwrap(), Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
        assert!(selection_rpc(&mut selection, pilot.character, "PrepareCharacterForDelete").await.unwrap().as_int().is_some());
        test_db.execute("UPDATE game.characters SET delete_prepare_at=1 WHERE character_id=$1", &[&pilot.character]).await.unwrap();
        let identity = Identity::of_client(&market);
        let request = BuyRequest { station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, range: -1, min_volume: 1, duration: 1, expected_brokers_fee: None };
        let bid = place_buy(&identity, &test_db, &market.world, &market.sde, request, None).await.unwrap();
        let owners = [corp];
        let characters = [pilot.character, ceo.character];
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        market.world.register_pilot(pilot.character, tx);
        let before_bid = fixture::market_fingerprint(&test_db, &owners, &characters).await.unwrap();
        assert_eq!(delete(&mut selection, pilot.character).await.unwrap(), Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
        assert_eq!(fixture::market_fingerprint(&test_db, &owners, &characters).await.unwrap(), before_bid);
        assert!(rx.try_recv().is_err());
        assert!(selection.outbox.is_empty());
        assert!(test_db.query_opt("SELECT order_id FROM game.market_orders WHERE order_id=$1", &[&bid.id]).await.unwrap().is_some());
        let item: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&ty, &pilot.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&identity, &test_db, &market.world, &market.sde, SellRequest { item_id: item, station_id: station, type_id: ty.into(), price: 200000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let ask: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&pilot.character]).await.unwrap().get("order_id");
        let before: String = test_db.query_one("SELECT row_to_json(c)::text AS row FROM game.characters c WHERE character_id=$1", &[&pilot.character]).await.unwrap().get("row");
        assert_eq!(delete(&mut selection, pilot.character).await.unwrap(), Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
        assert_eq!(test_db.query_one("SELECT row_to_json(c)::text AS row FROM game.characters c WHERE character_id=$1", &[&pilot.character]).await.unwrap().get::<_, String>("row"), before);
        cancel_order(&identity, &test_db, &market.world, &market.sde, bid.id, region).await.unwrap();
        while rx.try_recv().is_ok() {}
        let before_ask = fixture::market_fingerprint(&test_db, &owners, &characters).await.unwrap();
        assert_eq!(delete(&mut selection, pilot.character).await.unwrap(), Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
        assert_eq!(fixture::market_fingerprint(&test_db, &owners, &characters).await.unwrap(), before_ask);
        assert!(rx.try_recv().is_err());
        assert!(selection.outbox.is_empty());
        cancel_order(&identity, &test_db, &market.world, &market.sde, ask, region).await.unwrap();
        let corporate = place_buy(&identity, &test_db, &market.world, &market.sde, request, Some(infinity_core::consts::ACCOUNT_CASH)).await.unwrap();
        assert_eq!(delete(&mut selection, pilot.character).await.unwrap(), Value::None);
        test_db.execute("UPDATE game.characters SET balance=10000000, station_id=$2, delete_prepare_at=1 WHERE character_id=$1", &[&ceo.character, &station]).await.unwrap();
        let (mut racer, _c) = client(test_db.clone(), ceo.character).await;
        racer.session.apply(vec![("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
        let (mut race_selection, _d) = client(test_db.clone(), 0).await;
        race_selection.login.user_id = ceo.account;
        let race_identity = Identity::of_client(&racer);
        let (deletion, placement) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(delete(&mut race_selection, ceo.character), place_buy(&race_identity, &test_db, &racer.world, &racer.sde, request, None))
        }).await.expect("placement and deletion deadlocked");
        match deletion.unwrap() {
            Value::None => assert!(placement.is_err()),
            value => {
                assert_eq!(value, Value::str(messages::MSG_CHARACTER_NOT_READY_FOR_DELETION));
                assert!(placement.is_ok());
            }
        }
        assert!(test_db.query_opt("SELECT order_id FROM game.market_orders WHERE order_id=$1", &[&corporate.id]).await.unwrap().is_some());
        assert!(place_buy(&identity, &test_db, &market.world, &market.sde, request, None).await.is_err());
    }).await;
    let owners = vec![pilot.character, ceo.character, corp];
    db.transaction(|tx, _: &mut ()| Box::pin(async move { fixture::cleanup_progress(tx, &[corp], &[corp]).await }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE char_id=ANY($1) OR corporation_id=$2", &[&owners, &corp]).await.unwrap();
    db.execute("DELETE FROM game.market_order_history WHERE char_id=ANY($1) OR corporation_id=$2", &[&owners, &corp]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&owners]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0 WHERE character_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, pilot).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    scenario.unwrap();
}
