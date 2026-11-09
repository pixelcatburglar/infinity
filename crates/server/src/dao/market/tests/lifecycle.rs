//! Real-database PLEX lifecycle and wire-scope regressions.
use super::*;
use crate::{dao::{accounts::plex, market::orders::{self, Order}}, services::{self, identity::Identity, market::{self, BuyRequest, SellRequest, trading::{place_buy, place_sell}}, tests::{client, fixture}}};
use marshal::Value;

fn request(o: &Order, price: f64) -> [Value; 9] {
    [Value::Int(o.id), Value::Float(price), Value::Bool(o.bid), Value::Int(o.at.id),
     Value::Int(o.at.system), Value::Float(o.price.wire_isk()), Value::Int(o.range),
     Value::Int(i64::from(o.vol_remaining)), Value::Int(o.issued.unwrap())]
}
async fn rpc(c: &mut crate::client::Client, method: &str, args: &[Value]) -> services::CallResult {
    let kwargs = Value::None;
    services::dispatch(c, &services::Call { service: "marketProxy", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}
fn buy(at: Station, price: f64, quantity: i64) -> BuyRequest {
    BuyRequest { station_id: at.id, type_id: i64::from(TYPE_PLEX), price, quantity, range: -1, min_volume: 1, duration: 30, expected_brokers_fee: None }
}
async fn fingerprint(db: &Db, person: Fixture, id: i64) -> (String, String, i64, i64) {
    let row = db.query_one("SELECT (SELECT row_to_json(o)::text FROM game.market_orders o WHERE order_id=$2) AS order_row, c.balance::text AS balance, (SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1) AS journals, (SELECT count(*) FROM game.plex_vault_transactions WHERE user_id=$3) AS vault_journals FROM game.characters c WHERE c.character_id=$1", &[&person.character, &id, &person.account]).await.unwrap();
    (row.get("order_row"), row.get("balance"), row.get("journals"), row.get("vault_journals"))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn modify_wire_scope_snapshot_and_cooldown() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (at, _) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), person.character).await;
        c.session.apply(vec![("stationid", Value::Int(at.id)), ("solarsystemid2", Value::Int(at.system))]);
        fund(&test_db, person, "100000000000.00").await;
        let identity = Identity::of_client(&c);
        let placed = place_buy(&identity, &test_db, &c.world, &c.sde, buy(at, 1000000000.0, 2), None).await.unwrap();
        let o = orders::my_order(&test_db, person.character, placed.id).await.unwrap().unwrap();
        let before = fingerprint(&test_db, person, o.id).await;
        let outbox = c.outbox.len();
        assert!(rpc(&mut c, "ModifyCharOrder", &request(&o, 1100000000.0)).await.is_err());
        for index in [5, 7, 8] {
            let mut stale = request(&o, 1000000000.0);
            stale[index] = match &stale[index] { Value::Int(v) => Value::Int(v + 1), Value::Float(v) => Value::Float(v + 1000000.0), _ => unreachable!() };
            assert!(rpc(&mut c, "ModifyPlexCharOrder", &stale).await.is_err());
        }
        assert!(rpc(&mut c, "ModifyPlexCharOrder", &request(&o, 1000000000.0)).await.is_ok());
        if crate::config::game().market.modification_delay_seconds > 0 {
            assert!(rpc(&mut c, "ModifyPlexCharOrder", &request(&o, 1100000000.0)).await.is_err());
        }
        assert_eq!(fingerprint(&test_db, person, o.id).await, before);
        assert_eq!(c.outbox.len(), outbox);
        age_order_for_modification(&test_db, o.id).await.unwrap();
        let aged = orders::my_order(&test_db, person.character, o.id).await.unwrap().unwrap();
        assert!(rpc(&mut c, "ModifyPlexCharOrder", &request(&aged, 1100000000.0)).await.is_ok());
        let changed = orders::my_order(&test_db, person.character, o.id).await.unwrap().unwrap();
        assert_eq!(changed.price.decimal(), "1100000000.00");
        assert_eq!(changed.escrow.decimal(), "2200000000.00");
        assert!(changed.issued.unwrap() > aged.issued.unwrap());
        assert_eq!(changed.expires_at.unwrap(), changed.issued.unwrap() + changed.duration * FILETIME_DAY);
        let regular_type = c.sde.types.iter().find(|ty| ty.type_id > 0 && ty.type_id != TYPE_PLEX && ty.published == Some(true) && ty.market_group_id.is_some() && c.sde.is_tradable(ty.type_id)).unwrap().type_id;
        let mut regional = changed.clone();
        regional.id = orders::next_order_id(&test_db).await.unwrap();
        regional.type_id = regular_type;
        regional.scope = orders::MarketScope::Regional;
        regional.vault_user_id = None;
        orders::insert(&test_db, &regional).await.unwrap();
        let before = fingerprint(&test_db, person, regional.id).await;
        let outbox = c.outbox.len();
        assert!(rpc(&mut c, "ModifyPlexCharOrder", &request(&regional, 1200000000.0)).await.is_err());
        assert_eq!(fingerprint(&test_db, person, regional.id).await, before);
        assert_eq!(c.outbox.len(), outbox);
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn cancellation_refunds_bound_vault_and_cash_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (at, other) = isolated_stations(&db, &probe.sde).await;
    let person = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), person.character).await;
        c.session.apply(vec![("stationid", Value::Int(at.id)), ("solarsystemid2", Value::Int(at.system))]);
        fund(&test_db, person, "100000000000.00").await;
        let identity = Identity::of_client(&c);
        let item = physical_plex(&test_db, person.character, at.id, 3).await;
        place_sell(&identity, &test_db, &c.world, &c.sde, SellRequest { item_id: item, station_id: at.id, type_id: i64::from(TYPE_PLEX), price: 1200000000.0, quantity: 3, duration: 30, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let sell = orders::my_orders(&test_db, person.character).await.unwrap().into_iter().find(|o| !o.bid).unwrap();
        assert_eq!(sell.vault_user_id, Some(person.account));
        assert_eq!(sell.plex_escrow, 3);
        let vault_before = plex::balance(&test_db, person.account).await.unwrap();
        assert_eq!(vault_before, "0");
        market::lifecycle::cancel_order(&identity, &test_db, &c.world, &c.sde, sell.id, other.region).await.unwrap();
        assert_eq!(plex::balance(&test_db, person.account).await.unwrap(), "3");
        let physical: i128 = test_db.query_one("SELECT COALESCE(sum(quantity),0)::text AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2", &[&person.character, &TYPE_PLEX]).await.unwrap().get::<_, &str>("quantity").parse().unwrap();
        assert_eq!(physical, 0);
        let bid = place_buy(&identity, &test_db, &c.world, &c.sde, buy(at, 1000000000.0, 2), None).await.unwrap();
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&person.character]).await.unwrap().get("balance");
        market::lifecycle::cancel_order(&identity, &test_db, &c.world, &c.sde, bid.id, other.region).await.unwrap();
        let refunded: bool = test_db.query_one("SELECT balance=$2::text::numeric+2000000000 AS ok FROM game.characters WHERE character_id=$1", &[&person.character, &before]).await.unwrap().get("ok");
        assert!(refunded);
        let after = fingerprint_history(&test_db, person).await;
        assert!(market::lifecycle::cancel_order(&identity, &test_db, &c.world, &c.sde, bid.id, other.region).await.is_err());
        assert!(market::lifecycle::cancel_order(&identity, &test_db, &c.world, &c.sde, sell.id, other.region).await.is_err());
        assert_eq!(fingerprint_history(&test_db, person).await, after);
        let reasons = test_db.query("SELECT close_reason,escrow::text AS escrow,plex_escrow,vol_remaining FROM game.market_order_history WHERE order_id=ANY($1) ORDER BY order_id", &[&vec![sell.id, bid.id]]).await.unwrap();
        assert_eq!(reasons.len(), 2);
        for row in reasons { assert_eq!(row.get::<_, String>("close_reason"), "cancelled"); assert_eq!(row.get::<_, String>("escrow"), "0.00"); assert_eq!(row.get::<_, i64>("plex_escrow"), 0); assert!(row.get::<_, i64>("vol_remaining") > 0); }
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}
async fn fingerprint_history(db: &Db, person: Fixture) -> (String, String, i64, i64) {
    let row = db.query_one("SELECT balance::text AS balance, (SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1) AS journals, (SELECT count(*) FROM game.plex_vault_transactions WHERE user_id=$2) AS vault_journals FROM game.characters WHERE character_id=$1", &[&person.character, &person.account]).await.unwrap();
    (row.get("balance"), plex::balance(db, person.account).await.unwrap(), row.get("journals"), row.get("vault_journals"))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn crossing_reprice_reserves_and_refunds_price_improvement() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (at, other) = isolated_stations(&db, &probe.sde).await;
    let buyer = fixture::create(&db).await.unwrap();
    let seller = match fixture::create(&db).await {
        Ok(value) => value,
        Err(error) => { fixture::cleanup(&db, buyer).await.unwrap(); panic!("second fixture: {error}") }
    };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut bc, _peer) = client(test_db.clone(), buyer.character).await;
        let (mut sc, _peer) = client(test_db.clone(), seller.character).await;
        bc.session.apply(vec![("stationid", Value::Int(at.id)), ("solarsystemid2", Value::Int(at.system))]);
        sc.session.apply(vec![("stationid", Value::Int(other.id)), ("solarsystemid2", Value::Int(other.system))]);
        fund(&test_db, buyer, "100000000000.00").await;
        fund(&test_db, seller, "100000000000.00").await;
        let bid = place_buy(&Identity::of_client(&bc), &test_db, &bc.world, &bc.sde, buy(at, 1000000000.0, 3), None).await.unwrap();
        let item = physical_plex(&test_db, seller.character, other.id, 1).await;
        place_sell(&Identity::of_client(&sc), &test_db, &sc.world, &sc.sde, SellRequest { item_id: item, station_id: other.id, type_id: i64::from(TYPE_PLEX), price: 1100000000.0, quantity: 1, duration: 30, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        age_order_for_modification(&test_db, bid.id).await.unwrap();
        let old = orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().unwrap();
        assert!(rpc(&mut bc, "ModifyPlexCharOrder", &request(&old, 1200000000.0)).await.is_ok());
        let current = orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().unwrap();
        assert_eq!(current.vol_remaining, 2);
        assert_eq!(current.escrow.decimal(), "2400000000.00");
        assert!(current.issued.unwrap() > old.issued.unwrap());
        assert_eq!(current.expires_at.unwrap(), current.issued.unwrap() + current.duration * FILETIME_DAY);
        let fill = test_db.query_one("SELECT price::text AS price,quantity FROM game.market_transactions WHERE buy_order_id=$1", &[&bid.id]).await.unwrap();
        assert_eq!(fill.get::<_, String>("price"), "1100000000.00");
        assert_eq!(fill.get::<_, i64>("quantity"), 1);
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "1");
        let entries = test_db.query_one("SELECT COALESCE(sum(j.amount) FILTER (WHERE j.entry_type_id=$2 AND j.amount>0),0)::text AS credits FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.order_id=$3", &[&buyer.character, &infinity_core::consts::REF_MARKET_ESCROW, &bid.id]).await.unwrap();
        assert_eq!(entries.get::<_, String>("credits"), "100000000.00");
        age_order_for_modification(&test_db, bid.id).await.unwrap();
        let old = orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().unwrap();
        assert!(rpc(&mut bc, "ModifyPlexCharOrder", &request(&old, 900000000.0)).await.is_ok());
        let reduced = orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().unwrap();
        assert_eq!(reduced.escrow.decimal(), "1800000000.00");
        let entries = test_db.query_one("SELECT COALESCE(sum(j.amount) FILTER (WHERE j.entry_type_id=$2 AND j.amount>0),0)::text AS credits FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.order_id=$3", &[&buyer.character, &infinity_core::consts::REF_MARKET_ESCROW, &bid.id]).await.unwrap();
        assert_eq!(entries.get::<_, String>("credits"), "700000000.00");
    }).await;
    cleanup(&db, &[buyer, seller]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book, no overdue player orders and external flock"]
async fn expiry_boundaries_and_repeated_handler_refund_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (at, _) = isolated_stations(&db, &probe.sde).await;
    assert!(orders::expired(&db, FILETIME_DAY, filetime_now()).await.unwrap().is_empty(), "dedicated database must have no overdue player orders");
    let person = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), person.character).await;
        c.session.apply(vec![("stationid", Value::Int(at.id)), ("solarsystemid2", Value::Int(at.system))]);
        fund(&test_db, person, "100000000000.00").await;
        let bid = place_buy(&Identity::of_client(&c), &test_db, &c.world, &c.sde, buy(at, 1000000000.0, 2), None).await.unwrap();
        let deadline = filetime_now() - FILETIME_SECOND;
        test_db.execute("UPDATE game.market_orders SET expires_at=$2::bigint, created_at=$2::bigint-$3::bigint, issued=$2::bigint-$3::bigint WHERE order_id=$1", &[&bid.id, &deadline, &FILETIME_DAY]).await.unwrap();
        for (now, expected) in [(deadline - 1, false), (deadline, true), (deadline + 1, true)] {
            let found = orders::expired(&test_db, FILETIME_DAY, now).await.unwrap().into_iter().any(|o| o.id == bid.id);
            assert_eq!(found, expected);
        }
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&person.character]).await.unwrap().get("balance");
        let job = crate::scheduler::Job { id: crate::scheduler::MARKET.into(), kind: crate::scheduler::MARKET.into(), due_at: deadline, payload: "{}".into() };
        market::lifecycle::expire_orders(test_db.clone(), c.world.clone(), job.clone()).await.unwrap();
        let refunded: bool = test_db.query_one("SELECT balance=$2::text::numeric+2000000000 AS ok FROM game.characters WHERE character_id=$1", &[&person.character, &before]).await.unwrap().get("ok");
        assert!(refunded);
        let after = fingerprint_history(&test_db, person).await;
        let (restarted, _peer) = client(test_db.clone(), person.character).await;
        market::lifecycle::expire_orders(test_db.clone(), restarted.world.clone(), job).await.unwrap();
        assert_eq!(fingerprint_history(&test_db, person).await, after);
        let history = test_db.query_one("SELECT close_reason,escrow::text AS escrow,vol_remaining FROM game.market_order_history WHERE order_id=$1", &[&bid.id]).await.unwrap();
        assert_eq!(history.get::<_, String>("close_reason"), "expired");
        assert_eq!(history.get::<_, String>("escrow"), "0.00");
        assert_eq!(history.get::<_, i64>("vol_remaining"), 2);
    }).await;
    cleanup(&db, &[person]).await;
    result.unwrap();
}
