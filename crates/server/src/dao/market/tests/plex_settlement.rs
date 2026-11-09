//! Real global PLEX settlement through market services and the dedicated database.
use super::*;
use crate::{dao::{accounts::plex, market::orders}, services::{identity::Identity, market::{BuyRequest, SellRequest, trading::{place_buy, place_sell}}, tests::{client, fixture}}};

fn identity(character: i64, station: Option<Station>, system: i64) -> Identity {
    Identity { char_id: character, ship_id: None, corp_id: 1, station_id: station.map(|s| s.id), system_id: Some(system) }
}
fn buy(station: Station, price: f64, quantity: i64, min_volume: i64, duration: i64) -> BuyRequest {
    BuyRequest { station_id: station.id, type_id: i64::from(TYPE_PLEX), price, quantity, range: -1, min_volume, duration, expected_brokers_fee: None }
}
fn sell(station: Station, item: i64, price: f64, quantity: i64, duration: i64) -> SellRequest {
    SellRequest { item_id: item, station_id: station.id, type_id: i64::from(TYPE_PLEX), price, quantity, duration, office_id: None, expected_brokers_fee: None }
}
async fn balance(db: &Db, character: i64) -> String {
    db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("balance")
}
async fn pair(db: &Db) -> [Fixture; 2] {
    let first = fixture::create(db).await.unwrap();
    match fixture::create(db).await {
        Ok(second) => [first, second],
        Err(error) => { fixture::cleanup(db, first).await.unwrap(); panic!("second fixture: {error}") }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn price_time_priority_and_same_wallet_skip_at_high_notional() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let people = pair(&db).await;
    let [seller, buyer] = people;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), seller.character).await;
        fund(&test_db, seller, "100000000000.00").await;
        fund(&test_db, buyer, "100000000000.00").await;
        let seller_id = identity(seller.character, Some(a), a.system);
        let buyer_id = identity(buyer.character, Some(b), b.system);
        let own = place_buy(&seller_id, &test_db, &c.world, &c.sde, buy(a, 1200000000.0, 1, 1, 30), None).await.unwrap();
        let first = place_buy(&buyer_id, &test_db, &c.world, &c.sde, buy(b, 1000000000.0, 1, 1, 30), None).await.unwrap();
        let second = place_buy(&buyer_id, &test_db, &c.world, &c.sde, buy(b, 1000000000.0, 1, 1, 30), None).await.unwrap();
        let best = place_buy(&buyer_id, &test_db, &c.world, &c.sde, buy(b, 1100000000.0, 1, 1, 30), None).await.unwrap();
        let item = physical_plex(&test_db, seller.character, a.id, 2).await;
        place_sell(&seller_id, &test_db, &c.world, &c.sde, sell(a, item, 1000000000.0, 2, 0), None).await.unwrap();
        let fills = test_db.query("SELECT buy_order_id,price::text AS price FROM game.market_transactions WHERE seller_id=$1 ORDER BY transaction_id", &[&seller.character]).await.unwrap();
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].get::<_, Option<i64>>("buy_order_id"), Some(best.id));
        assert_eq!(fills[0].get::<_, String>("price"), "1100000000.00");
        assert_eq!(fills[1].get::<_, Option<i64>>("buy_order_id"), Some(first.id));
        assert_eq!(fills[1].get::<_, String>("price"), "1000000000.00");
        assert!(orders::my_order(&test_db, buyer.character, first.id).await.unwrap().is_none());
        assert_eq!(orders::my_order(&test_db, buyer.character, second.id).await.unwrap().unwrap().vol_remaining, 1);
        assert_eq!(orders::my_order(&test_db, seller.character, own.id).await.unwrap().unwrap().escrow.decimal(), "1200000000.00");
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "2");
        assert_eq!(plex::balance(&test_db, seller.account).await.unwrap(), "0");
        for person in [seller, buyer] {
            let difference: String = test_db.query_one("SELECT (c.balance - 100000000000.00 - COALESCE((SELECT sum(j.amount) FROM game.wallet_journal j JOIN game.wallet_accounts w USING(account_id) WHERE w.owner_id=c.character_id),0))::text AS difference FROM game.characters c WHERE c.character_id=$1", &[&person.character]).await.unwrap().get("difference");
            assert_eq!(difference, "0.00");
        }
    }).await;
    cleanup(&db, &people).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn physical_sell_cross_region_space_buy_settles_exactly_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let people = pair(&db).await;
    let [seller, buyer] = people;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), seller.character).await;
        fund(&test_db, seller, "1000000.00").await;
        fund(&test_db, buyer, "1000000.00").await;
        let item = physical_plex(&test_db, seller.character, a.id, 2).await;
        place_sell(&identity(seller.character, Some(a), a.system), &test_db, &c.world, &c.sde, sell(a, item, 12.34, 2, 30), None).await.unwrap();
        let ask = orders::my_orders(&test_db, seller.character).await.unwrap().pop().unwrap();
        assert_eq!((ask.plex_escrow, ask.vol_remaining, ask.vault_user_id), (2, 2, Some(seller.account)));
        assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().is_none());
        let order = place_buy(&identity(buyer.character, None, b.system), &test_db, &c.world, &c.sde, buy(b, 15.0, 2, 1, 30), None).await.unwrap();
        assert_eq!(order.vol_remaining, 0);
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "2");
        assert_eq!(balance(&test_db, buyer.character).await, "999873.82");
        assert_eq!(balance(&test_db, seller.character).await, "999921.48");
        let row = test_db.query_one("SELECT t.transaction_id,t.buyer_id,t.seller_id,t.quantity,t.price::text AS price,t.station_id,t.buyer_vault_user_id,t.seller_vault_user_id,ba.owner_id AS buyer_owner,sa.owner_id AS seller_owner FROM game.market_transactions t JOIN game.wallet_accounts ba ON ba.account_id=t.buyer_account_id JOIN game.wallet_accounts sa ON sa.account_id=t.seller_account_id WHERE t.buy_order_id=$1 AND t.sell_order_id=$2", &[&order.id,&ask.id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("buyer_id"), buyer.character);
        assert_eq!(row.get::<_, i64>("seller_id"), seller.character);
        assert_eq!(row.get::<_, i64>("buyer_owner"), buyer.character);
        assert_eq!(row.get::<_, i64>("seller_owner"), seller.character);
        assert_eq!(row.get::<_, i64>("quantity"), 2);
        assert_eq!(row.get::<_, String>("price"), "12.34");
        assert_eq!(row.get::<_, i64>("station_id"), a.id);
        assert_eq!(row.get::<_, Option<i64>>("buyer_vault_user_id"), Some(buyer.account));
        assert_eq!(row.get::<_, Option<i64>>("seller_vault_user_id"), Some(seller.account));
        let fill: i64 = row.get("transaction_id");
        let ledger = test_db.query("SELECT delta::text AS delta,order_id,trade_id FROM game.plex_vault_transactions WHERE user_id=$1 AND kind='market_fill'", &[&buyer.account]).await.unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].get::<_, String>("delta"), "2");
        assert_eq!(ledger[0].get::<_, Option<i64>>("order_id"), Some(order.id));
        assert_eq!(ledger[0].get::<_, Option<i64>>("trade_id"), Some(fill));
        assert!(orders::my_orders(&test_db, seller.character).await.unwrap().is_empty());
        assert!(orders::my_orders(&test_db, buyer.character).await.unwrap().is_empty());
    }).await;
    cleanup(&db, &people).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn bid_escrow_partial_tail_and_overflow_rollback() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let people = pair(&db).await;
    let [seller, buyer] = people;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), seller.character).await;
        fund(&test_db, seller, "1000000.00").await;
        fund(&test_db, buyer, "1000000.00").await;
        let bid = place_buy(&identity(buyer.character, Some(b), b.system), &test_db, &c.world, &c.sde, buy(b, 12.34, 3, 2, 30), None).await.unwrap();
        assert_eq!(bid.escrow.decimal(), "37.02");
        assert_eq!(balance(&test_db, buyer.character).await, "999861.13");
        let item = physical_plex(&test_db, seller.character, a.id, 3).await;
        let seller_id = identity(seller.character, Some(a), a.system);
        assert!(place_sell(&seller_id, &test_db, &c.world, &c.sde, sell(a, item, 12.34, 1, 0), None).await.is_err());
        assert_eq!(orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().unwrap().vol_remaining, 3);
        place_sell(&seller_id, &test_db, &c.world, &c.sde, sell(a, item, 12.34, 2, 0), None).await.unwrap();
        let remaining = orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().unwrap();
        assert_eq!((remaining.vol_remaining, remaining.escrow.decimal()), (1, "12.34".to_string()));
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "2");
        place_sell(&seller_id, &test_db, &c.world, &c.sde, sell(a, item, 12.34, 1, 0), None).await.unwrap();
        assert!(orders::my_order(&test_db, buyer.character, bid.id).await.unwrap().is_none());
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "3");
        assert_eq!(balance(&test_db, buyer.character).await, "999861.13");
        assert_eq!(balance(&test_db, seller.character).await, "1000034.06");
        let extra = physical_plex(&test_db, seller.character, a.id, 1).await;
        place_sell(&seller_id, &test_db, &c.world, &c.sde, sell(a, extra, 12.34, 1, 30), None).await.unwrap();
        crate::dao::accounts::plex::tests::set_balance(&test_db, buyer.account, "999999999999999999999999999999999999").await.unwrap();
        let before_buyer = balance(&test_db, buyer.character).await;
        let before_seller = balance(&test_db, seller.character).await;
        let before_orders = orders::my_orders(&test_db, seller.character).await.unwrap();
        assert!(place_buy(&identity(buyer.character, None, b.system), &test_db, &c.world, &c.sde, buy(b, 12.34, 1, 1, 0), None).await.is_err());
        assert_eq!(balance(&test_db, buyer.character).await, before_buyer);
        assert_eq!(balance(&test_db, seller.character).await, before_seller);
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "999999999999999999999999999999999999");
        let after_orders = orders::my_orders(&test_db, seller.character).await.unwrap();
        assert_eq!(before_orders.len(), 1);
        assert_eq!(after_orders.len(), 1);
        assert_eq!((after_orders[0].id, after_orders[0].vol_remaining, after_orders[0].plex_escrow), (before_orders[0].id, 1, 1));
        crate::dao::accounts::plex::tests::set_balance(&test_db, buyer.account, "100000000000000000000000").await.unwrap();
        let constraint = format!("plex_settlement_{}", buyer.account);
        test_db.execute(&format!("ALTER TABLE game.plex_vault_transactions ADD CONSTRAINT {constraint} CHECK (user_id <> {} OR kind <> 'market_fill') NOT VALID", buyer.account), &[]).await.unwrap();
        let before = test_db.query_one("SELECT (SELECT count(*) FROM game.wallet_journal WHERE actor_id=ANY($1)) AS journals, (SELECT count(*) FROM game.plex_vault_transactions WHERE user_id=$2) AS vault_rows, (SELECT count(*) FROM game.progress_events WHERE subject_character_id=ANY($1)) AS events", &[&vec![buyer.character,seller.character], &buyer.account]).await.unwrap();
        let buyer_cash = balance(&test_db, buyer.character).await;
        let seller_cash = balance(&test_db, seller.character).await;
        assert!(place_buy(&identity(buyer.character, None, b.system), &test_db, &c.world, &c.sde, buy(b, 12.34, 1, 1, 0), None).await.is_err());
        let after = test_db.query_one("SELECT (SELECT count(*) FROM game.wallet_journal WHERE actor_id=ANY($1)) AS journals, (SELECT count(*) FROM game.plex_vault_transactions WHERE user_id=$2) AS vault_rows, (SELECT count(*) FROM game.progress_events WHERE subject_character_id=ANY($1)) AS events", &[&vec![buyer.character,seller.character], &buyer.account]).await.unwrap();
        for name in ["journals", "vault_rows", "events"] { assert_eq!(before.get::<_, i64>(name), after.get::<_, i64>(name)); }
        assert_eq!(balance(&test_db, buyer.character).await, buyer_cash);
        assert_eq!(balance(&test_db, seller.character).await, seller_cash);
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "100000000000000000000000");
        assert_eq!(orders::my_orders(&test_db, seller.character).await.unwrap()[0].plex_escrow, 1);
        test_db.execute(&format!("ALTER TABLE game.plex_vault_transactions DROP CONSTRAINT {constraint}"), &[]).await.unwrap();
        place_buy(&identity(buyer.character, None, b.system), &test_db, &c.world, &c.sde, buy(b, 12.34, 1, 1, 0), None).await.unwrap();
        assert_eq!(plex::balance(&test_db, buyer.account).await.unwrap(), "100000000000000000000001");
        let fills: i64 = test_db.query_one("SELECT count(*) AS n FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$2", &[&buyer.character, &seller.character]).await.unwrap().get("n");
        assert_eq!(fills, 3);
    }).await;
    db.execute(&format!("ALTER TABLE game.plex_vault_transactions DROP CONSTRAINT IF EXISTS plex_settlement_{}", buyer.account), &[]).await.unwrap();
    cleanup(&db, &people).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE, empty global PLEX book and external flock"]
async fn wide_global_fill_and_cancel_refund() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (probe, _peer) = client(db.clone(), 1).await;
    let (a, b) = isolated_stations(&db, &probe.sde).await;
    let people = pair(&db).await;
    let [seller, buyer] = people;
    let work = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(work.clone(), seller.character).await;
        fund(&work, seller, "100000000000.00").await;
        fund(&work, buyer, "100000000000.00").await;
        let quantity = i64::from(i32::MAX) + 1;
        let item = physical_plex(&work, seller.character, a.id, quantity * 2).await;
        let seller_id = identity(seller.character, Some(a), a.system);
        assert!(place_sell(&seller_id, &work, &c.world, &c.sde, sell(a, item, 1.0, quantity * 2, 30), None).await.unwrap());
        let ask = orders::my_orders(&work, seller.character).await.unwrap().pop().unwrap();
        assert_eq!(ask.plex_escrow, quantity * 2);
        place_buy(&identity(buyer.character, None, b.system), &work, &c.world, &c.sde, buy(b, 1.0, quantity, 1, 0), None).await.unwrap();
        let remaining = orders::my_order(&work, seller.character, ask.id).await.unwrap().unwrap();
        assert_eq!((remaining.vol_remaining, remaining.plex_escrow), (quantity, quantity));
        assert_eq!(plex::balance(&work, buyer.account).await.unwrap(), quantity.to_string());
        let row = work.query_one("SELECT quantity FROM game.market_transactions WHERE sell_order_id=$1", &[&ask.id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("quantity"), quantity);
        crate::services::market::lifecycle::cancel_order(&seller_id, &work, &c.world, &c.sde, ask.id, b.region).await.unwrap();
        assert_eq!(plex::balance(&work, seller.account).await.unwrap(), quantity.to_string());
        assert!(orders::my_order(&work, seller.character, ask.id).await.unwrap().is_none());
    }).await;
    cleanup(&db, &people).await;
    result.unwrap();
}
