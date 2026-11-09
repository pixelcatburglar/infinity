//! Real database coverage for NPC fills and escrow order lifecycles.
use super::*;
use super::super::{lifecycle, trading::{place_buy, place_sell}};
use infinity_core::consts::{FLAG_HANGAR, REF_TRANSACTION_TAX};
use crate::services::{identity::Identity, tests::{client, fixture}};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn npc_orders_use_only_player_accounts_and_single_public_fills() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let player = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), player.character).await;
    let at = fixture::isolated_market_stations(&db, &c.sde).await.unwrap().next().expect("isolated NPC station");
    c.session.apply(vec![("stationid", Value::Int(at.id)), ("solarsystemid2", Value::Int(at.system))]);
    let ty = fixture::market_type_ids(&c.sde).next().expect("tradable market SDE type");
    let mut order = Order { id: orders::next_order_id(&db).await.unwrap(), type_id: ty, char_id: None, corporation_id: None, wallet_key: ACCOUNT_CASH, source_location_id: None, source_flag_id: None, at, price: crate::isk::IskAmount::from_whole_isk(100000).unwrap(), vol_entered: 1, vol_remaining: 1, min_volume: 1, bid: false, range: RANGE_STATION, duration: 1, issued: None, escrow: crate::isk::IskAmount::ZERO, scope: orders::MarketScope::for_type(ty), vault_user_id: None, plex_escrow: 0, created_at: None, expires_at: None };
    let ask = orders::insert(&db, &order).await.unwrap();
    order.bid = true;
    order.id = orders::next_order_id(&db).await.unwrap();
    let bid = orders::insert(&db, &order).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=$1", &[&player.character]).await.unwrap();
        let npc_accounts: i64 = test_db.query_one("SELECT count(*) AS n FROM game.wallet_accounts WHERE owner_id=$1", &[&at.owner]).await.unwrap().get("n");
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&ty,&player.character,&at.id,&FLAG_HANGAR]).await.unwrap().get("item_id");
        place_buy(&Identity::of_client(&c), &c.db, &c.world, &c.sde, BuyRequest { station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 2, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        place_sell(&Identity::of_client(&c), &c.db, &c.world, &c.sde, SellRequest { item_id: item, station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 2, duration: 0, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let fills = test_db.query("SELECT buyer_id,seller_id,buyer_account_id,seller_account_id,quantity FROM game.market_transactions WHERE order_id=ANY($1) ORDER BY order_id", &[&vec![ask,bid]]).await.unwrap();
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].get::<_, i64>("buyer_id"), player.character);
        assert_eq!(fills[0].get::<_, i64>("seller_id"), at.owner);
        assert!(fills[0].get::<_, Option<i64>>("buyer_account_id").is_some());
        assert_eq!(fills[0].get::<_, Option<i64>>("seller_account_id"), None);
        assert_eq!(fills[1].get::<_, i64>("buyer_id"), at.owner);
        let account: i64 = test_db.query_one("SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1 AND account_key=$2", &[&player.character,&ACCOUNT_CASH]).await.unwrap().get("account_id");
        assert_eq!(fills[0].get::<_, Option<i64>>("buyer_account_id"), Some(account));
        assert_eq!(fills[1].get::<_, Option<i64>>("seller_account_id"), Some(account));
        let npc_accounts_after: i64 = test_db.query_one("SELECT count(*) AS n FROM game.wallet_accounts WHERE owner_id=$1", &[&at.owner]).await.unwrap().get("n");
        assert_eq!(npc_accounts, npc_accounts_after);
        assert_eq!(fills[1].get::<_, i64>("seller_id"), player.character);
        assert_eq!(fills[1].get::<_, Option<i64>>("buyer_account_id"), None);
        assert!(fills[1].get::<_, Option<i64>>("seller_account_id").is_some());
        for row in fills { assert_eq!(row.get::<_, i64>("quantity"), 2); }
        let rows = test_db.query("SELECT vol_remaining FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask,bid]]).await.unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows { assert_eq!(row.get::<_, i64>("vol_remaining"), 1); }
        let foreign: i64 = test_db.query_one("SELECT count(*) AS n FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) JOIN game.market_transactions t ON t.transaction_id=j.market_transaction_id WHERE t.order_id=ANY($1) AND a.owner_id<>$2", &[&vec![ask,bid],&player.character]).await.unwrap().get("n");
        assert_eq!(foreign, 0);
        let history = listings::price_history(&test_db, at.region, &[ty], FILETIME_DAY, 0).await.unwrap();
        assert_eq!(history.iter().map(|h| h.volume).sum::<i128>(), 4);
        assert_eq!(history.iter().map(|h| h.count).sum::<i64>(), 2);
        let journal = test_db.query_one("SELECT count(*) FILTER (WHERE j.entry_type_id=$2) AS gross, count(*) FILTER (WHERE j.entry_type_id=$3) AS tax, (sum(j.amount) - (c.balance-10000000))::text AS difference FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) JOIN game.characters c ON c.character_id=a.owner_id WHERE a.owner_id=$1 GROUP BY c.balance", &[&player.character,&REF_MARKET_TRANSACTION,&REF_TRANSACTION_TAX]).await.unwrap();
        assert_eq!(journal.get::<_, i64>("gross"), 2);
        assert_eq!(journal.get::<_, i64>("tax"), 1);
        assert_eq!(journal.get::<_, String>("difference"), "0.00");
    }).await;
    cleanup_market_fixture(&db, player).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask,bid]]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn partial_escrow_reprice_improvement_cancel_and_expiry_are_once_only() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let buyer = fixture::create(&db).await.unwrap();
    let seller = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut bc, _bp) = client(test_db.clone(), buyer.character).await;
        let (mut sc, _sp) = client(test_db.clone(), seller.character).await;
        let at = fixture::isolated_market_stations(&test_db, &bc.sde).await.unwrap().next().expect("isolated NPC station");
        for c in [&mut bc, &mut sc] { c.session.apply(vec![("stationid", Value::Int(at.id)), ("solarsystemid2", Value::Int(at.system))]); }
        let ty = fixture::market_type_ids(&bc.sde).next().expect("tradable market SDE type");
        for id in [buyer.character, seller.character] { test_db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=$1", &[&id]).await.unwrap(); }
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,3,false) RETURNING item_id", &[&ty,&seller.character,&at.id,&FLAG_HANGAR]).await.unwrap().get("item_id");
        place_buy(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, BuyRequest { station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 4, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        let order: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND bid", &[&buyer.character]).await.unwrap().get("order_id");
        let before = test_db.query_one("SELECT c.balance::text AS balance,(SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1) AS journals FROM game.characters c WHERE c.character_id=$1", &[&buyer.character]).await.unwrap();
        place_sell(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, SellRequest { item_id: item, station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 0, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let after = test_db.query_one("SELECT c.balance::text AS balance,(SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1) AS journals FROM game.characters c WHERE c.character_id=$1", &[&buyer.character]).await.unwrap();
        assert_eq!(before.get::<_, String>("balance"), after.get::<_, String>("balance"));
        assert_eq!(before.get::<_, i64>("journals"), after.get::<_, i64>("journals"));
        let row = test_db.query_one("SELECT vol_remaining,escrow::text AS escrow FROM game.market_orders WHERE order_id=$1", &[&order]).await.unwrap();
        assert_eq!(row.get::<_, i64>("vol_remaining"), 3);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("escrow")).unwrap(), crate::isk::IskAmount::from_whole_isk(300000).unwrap());
        let own_item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&ty,&buyer.character,&at.id,&FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, SellRequest { item_id: own_item, station_id: at.id, type_id: ty.into(), price: 80000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let own_ask: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&buyer.character]).await.unwrap().get("order_id");
        let self_fills: i64 = test_db.query_one("SELECT count(*) AS n FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$1", &[&buyer.character]).await.unwrap().get("n");
        assert_eq!(self_fills, 0);
        crate::dao::market::tests::age_order_for_modification(&test_db, order).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, modify_request(&bc, order, 120000.0).await).await.unwrap();
        crate::dao::market::tests::age_order_for_modification(&test_db, order).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, modify_request(&bc, order, 90000.0).await).await.unwrap();
        place_sell(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, SellRequest { item_id: item, station_id: at.id, type_id: ty.into(), price: 95000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        crate::dao::market::tests::age_order_for_modification(&test_db, order).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, modify_request(&bc, order, 110000.0).await).await.unwrap();
        let row = test_db.query_one("SELECT vol_remaining,escrow::text AS escrow FROM game.market_orders WHERE order_id=$1", &[&order]).await.unwrap();
        assert_eq!(row.get::<_, i64>("vol_remaining"), 2);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("escrow")).unwrap(), crate::isk::IskAmount::from_whole_isk(220000).unwrap());
        let seller_journal = test_db.query_one("SELECT count(*) FILTER (WHERE j.entry_type_id=$2) AS gross, count(*) FILTER (WHERE j.entry_type_id=$3) AS tax, (sum(j.amount)-(c.balance-10000000))::text AS difference FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) JOIN game.characters c ON c.character_id=a.owner_id WHERE a.owner_id=$1 GROUP BY c.balance", &[&seller.character,&REF_MARKET_TRANSACTION,&REF_TRANSACTION_TAX]).await.unwrap();
        assert_eq!(seller_journal.get::<_, i64>("gross"), 2);
        assert_eq!(seller_journal.get::<_, i64>("tax"), 2);
        assert_eq!(seller_journal.get::<_, String>("difference"), "0.00");
        let stock = test_db.query_one("SELECT (SELECT quantity FROM game.items WHERE item_id=$1) AS seller_stock, (SELECT COALESCE(sum(quantity),0)::text FROM game.items WHERE owner_id=$2 AND type_id=$3 AND location_id=$4 AND flag_id=$5) AS buyer_stock", &[&item,&buyer.character,&ty,&at.id,&FLAG_HANGAR]).await.unwrap();
        assert_eq!(stock.get::<_, i64>("seller_stock"), 1);
        assert_eq!(stock.get::<_, String>("buyer_stock"), "2");
        let self_fills: i64 = test_db.query_one("SELECT count(*) AS n FROM game.market_transactions WHERE order_id=$1", &[&own_ask]).await.unwrap().get("n");
        assert_eq!(self_fills, 0);
        lifecycle::cancel_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, order, at.region).await.unwrap();
        assert!(lifecycle::cancel_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, order, at.region).await.is_err());
        let j = test_db.query_one("SELECT count(*) FILTER (WHERE j.entry_type_id=$2) AS escrow_count, sum(j.amount) FILTER (WHERE j.entry_type_id=$2)::text AS escrow_sum, count(*) FILTER (WHERE j.entry_type_id=$3) AS fees, (sum(j.amount)-(c.balance-10000000))::text AS difference FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) JOIN game.characters c ON c.character_id=a.owner_id WHERE a.owner_id=$1 GROUP BY c.balance", &[&buyer.character,&REF_MARKET_ESCROW,&REF_BROKER_FEE]).await.unwrap();
        assert_eq!(j.get::<_, i64>("escrow_count"), 6);
        assert_eq!(j.get::<_, String>("escrow_sum"), "-195000.00");
        assert_eq!(j.get::<_, i64>("fees"), 5);
        assert_eq!(j.get::<_, String>("difference"), "0.00");
        let history = test_db.query_one("SELECT count(*) AS n FROM game.market_order_history WHERE order_id=$1 AND state=$2", &[&order,&STATE_CANCELLED]).await.unwrap();
        assert_eq!(history.get::<_, i64>("n"), 1);
        place_buy(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, BuyRequest { station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 2, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        let due: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND bid", &[&buyer.character]).await.unwrap().get("order_id");
        test_db.execute("UPDATE game.market_orders SET issued=$2, expires_at=$2 WHERE order_id=$1", &[&due,&(filetime_now()-2*FILETIME_DAY)]).await.unwrap();
        for _ in 0..2 {
            lifecycle::expire_orders(test_db.clone(), Arc::clone(&bc.world), crate::scheduler::Job { id: "market-test".into(), kind: crate::scheduler::MARKET.into(), due_at: 0, payload: "{}".into() }).await.unwrap();
        }
        let row = test_db.query_one("SELECT (SELECT count(*) FROM game.market_orders WHERE order_id=$1) AS open, (SELECT count(*) FROM game.market_order_history WHERE order_id=$1 AND state=$2) AS closed, (SELECT count(*) FROM game.wallet_journal WHERE order_id=$1 AND entry_type_id=$3 AND amount>0) AS refunds, (SELECT COALESCE(sum(amount),0)::text FROM game.wallet_journal WHERE order_id=$1 AND entry_type_id=$3) AS net", &[&due,&STATE_CLOSED,&REF_MARKET_ESCROW]).await.unwrap();
        assert_eq!(row.get::<_, i64>("open"), 0);
        assert_eq!(row.get::<_, i64>("closed"), 1);
        assert_eq!(row.get::<_, i64>("refunds"), 1);
        assert_eq!(row.get::<_, String>("net"), "0.00");
    }).await;
    cleanup_market_fixture(&db, seller).await.unwrap();
    cleanup_market_fixture(&db, buyer).await.unwrap();
    result.unwrap();
}
