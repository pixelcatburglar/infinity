//! Personal and corporate immediate buys and sells accept whole-cent prices, while
//! resting orders and reprices require ticks. Mirrors `marketsvc.py` BuyStuff,
//! `buyMultiFromBase.py`, `sellMulti.py` and `marketutil/ticks.py`.
//! Uses CorporateFixture owners, per-test NPC orders, market transactions, wallets
//! and items. The real client UI is not covered.
use super::*;
use super::corporate_fixture::CorporateFixture;
use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, FLAG_HANGAR, REF_MARKET_TRANSACTION, REF_TRANSACTION_TAX};
use crate::services::tests::{client, fixture};

async fn place_buy_rpc(c: &mut Client, station: i64, ty: i32, price: f64, quantity: i64, duration: i64, corporate: bool) -> CallResult {
    let args = [Value::Int(station), Value::Int(i64::from(ty)), Value::Float(price), Value::Int(quantity), Value::Int(RANGE_STATION), Value::Int(1), Value::Int(duration), Value::Bool(corporate), Value::None];
    let kwargs = Value::None;
    let call = Call { service: "marketProxy", method: "PlaceBuyOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    super::super::market_proxy::dispatch(c, &call).await.unwrap()
}

async fn npc_order(db: &Db, at: Station, type_id: i32, price: &str, bid: bool) -> Order {
    Order { id: orders::next_order_id(db).await.unwrap(), type_id, char_id: None, corporation_id: None, wallet_key: ACCOUNT_CASH, source_location_id: None, source_flag_id: None, at, price: crate::isk::IskAmount::parse_decimal(price).unwrap(), vol_entered: 1_000_000, vol_remaining: 1_000_000, min_volume: 1, bid, range: RANGE_STATION, duration: 1, issued: None, escrow: crate::isk::IskAmount::ZERO, scope: orders::MarketScope::for_type(type_id), vault_user_id: None, plex_escrow: 0, created_at: None, expires_at: None }
}

fn assert_invalid_price(result: CallResult) {
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::INVALID_PRICE))]);
        }
        other => panic!("expected invalid price, got {other:?}"),
    }
}

fn assert_no_match(result: CallResult) {
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, messages::MSG_MKT_ORDER_DID_NOT_MATCH);
            assert!(dict.is_empty());
        }
        other => panic!("expected unmatched order, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn immediate_cent_buys_fill_npc_asks_at_the_ask_price() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let (buyer, _peer) = client(db.clone(), f.issuer.character).await;
    let second = fixture::market_type_ids(&buyer.sde).find(|&ty| ty != f.type_id).expect("second tradable market SDE type");
    drop(buyer);
    let ask_a = orders::insert(&db, &npc_order(&db, f.at, f.type_id, "1971.99", false).await).await.unwrap();
    let ask_b = orders::insert(&db, &npc_order(&db, f.at, second, "225005.00", false).await).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _peer) = client(test_db.clone(), f.issuer.character).await;
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        test_db.execute("UPDATE game.characters SET balance=1000000000000 WHERE character_id=$1", &[&f.issuer.character]).await.unwrap();
        let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        buyer.world.register_pilot(f.issuer.character, tx);
        assert!(place_buy_rpc(&mut buyer, f.at.id, f.type_id, 1971.99, 1_000_000, 0, false).await.is_ok());
        assert!(place_buy_rpc(&mut buyer, f.at.id, second, 225005.00, 1_000_000, 0, false).await.is_ok());
        assert!(place_buy_rpc(&mut buyer, f.at.id, second, 225005.00, 1111, 0, false).await.is_ok());
        let response = buy_rpc(&mut buyer, f.at.id, vec![buy_line(f.at.id, f.type_id, 1971.99, 5), buy_line(f.at.id, second, 225005.00, 7)], Value::Bool(false)).await.unwrap();
        let rows = response.items().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| crate::services::value_field(row, "volRemaining") == Some(&Value::Int(0))));
        let fills = test_db.query("SELECT type_id, quantity, price::text AS price FROM game.market_transactions WHERE buyer_id=$1", &[&f.issuer.character]).await.unwrap();
        let mut fills: Vec<_> = fills.iter().map(|row| (row.get::<_, i32>("type_id"), row.get::<_, i64>("quantity"), row.get::<_, String>("price"))).collect();
        let mut expected = vec![(f.type_id, 5, "1971.99".into()), (f.type_id, 1_000_000, "1971.99".into()), (second, 7, "225005.00".into()), (second, 1111, "225005.00".into()), (second, 1_000_000, "225005.00".into())];
        fills.sort();
        expected.sort();
        assert_eq!(fills, expected);
        let balance = test_db.query_one("SELECT (1000000000000::numeric - balance)::text AS spent FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap();
        assert_eq!(balance.get::<_, String>("spent"), "227228555449.95");
        for (ty, quantity) in [(f.type_id, 1_000_005_i64), (second, 1_001_118)] {
            assert_eq!(test_db.query_one("SELECT COALESCE(sum(quantity),0)::text AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=$3 AND flag_id=$4", &[&f.issuer.character, &ty, &f.at.id, &FLAG_HANGAR]).await.unwrap().get::<_, String>("quantity").parse::<i128>().unwrap(), i128::from(quantity));
        }
        let npc = test_db.query("SELECT vol_remaining FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask_a, ask_b]]).await.unwrap();
        assert_eq!(npc.len(), 2);
        assert!(npc.iter().all(|row| row.get::<_, i64>("vol_remaining") == 1_000_000));
        let history = test_db.query("SELECT vol_remaining FROM game.market_order_history WHERE char_id=$1 AND duration=0", &[&f.issuer.character]).await.unwrap();
        assert_eq!(history.len(), 5);
        assert!(history.iter().all(|row| row.get::<_, i64>("vol_remaining") == 0));
        assert!(std::iter::from_fn(|| notices.try_recv().ok()).any(|(name, _)| name == "OnOwnOrdersChanged"));
        buyer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH + 1)))]);
        assert!(place_buy_rpc(&mut buyer, f.at.id, f.type_id, 1971.99, 3, 0, true).await.is_ok());
        let response = buy_rpc(&mut buyer, f.at.id, vec![buy_line(f.at.id, f.type_id, 1971.99, 2)], Value::Bool(true)).await.unwrap();
        let rows = response.items().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(crate::services::value_field(&rows[0], "volRemaining"), Some(&Value::Int(0)));
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.corporation, &f.at.id, &FLAG_CORP_DELIVERIES, &f.type_id]).await.unwrap().get::<_, i64>("quantity"), 5);
        let key = ACCOUNT_CASH + 1;
        let spent = test_db.query_one("SELECT (10000000::numeric - balance)::text AS spent FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap();
        assert_eq!(spent.get::<_, String>("spent"), "9859.95");
    }).await;
    db.execute("DELETE FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask_a, ask_b]]).await.unwrap();
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn immediate_cent_sell_fills_matching_npc_bid_only() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let bid = orders::insert(&db, &npc_order(&db, f.at, f.type_id, "1971.99", true).await).await.unwrap();
    let wide = orders::insert(&db, &npc_order(&db, f.at, f.type_id, "1971.98", true).await).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut seller, _peer) = client(test_db.clone(), f.issuer.character).await;
        seller.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,10,false) RETURNING item_id", &[&f.type_id, &f.issuer.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let result = sell_rpc(&mut seller, vec![sell_line(f.at.id, f.type_id, item, 1971.99, 4, None)], false, 0).await.unwrap();
        assert_eq!(result, Value::Bool(true));
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i64>("quantity"), 6);
        let fill = test_db.query_one("SELECT quantity, price::text AS price FROM game.market_transactions WHERE seller_id=$1 AND order_id=$2 AND quantity=4", &[&f.issuer.character, &bid]).await.unwrap();
        assert_eq!(fill.get::<_, i64>("quantity"), 4);
        assert_eq!(fill.get::<_, String>("price"), "1971.99");
        let journal = test_db.query_one("SELECT amount::text AS amount FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$2 AND j.order_id=$3", &[&f.issuer.character, &REF_MARKET_TRANSACTION, &bid]).await.unwrap();
        assert_eq!(journal.get::<_, String>("amount"), "7887.96");
        let tax = test_db.query_one("SELECT count(*) AS n FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$2 AND j.order_id=$3", &[&f.issuer.character, &REF_TRANSACTION_TAX, &bid]).await.unwrap();
        assert_eq!(tax.get::<_, i64>("n"), 1);
        let corp_item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,5,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        let key = ACCOUNT_CASH + 1;
        seller.session.apply(vec![("corpid", Value::Int(f.corporation)), ("corpAccountKey", Value::Int(i64::from(key)))]);
        assert_eq!(sell_rpc(&mut seller, vec![sell_line(f.at.id, f.type_id, corp_item, 1971.99, 2, Some(f.office))], true, 0).await.unwrap(), Value::Bool(true));
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&corp_item]).await.unwrap().get::<_, i64>("quantity"), 3);
        let fill = test_db.query_one("SELECT quantity, price::text AS price FROM game.market_transactions WHERE seller_id=$1 AND order_id=$2 AND quantity=2", &[&f.issuer.character, &bid]).await.unwrap();
        assert_eq!(fill.get::<_, i64>("quantity"), 2);
        assert_eq!(fill.get::<_, String>("price"), "1971.99");
        let journal = test_db.query_one("SELECT amount::text AS amount FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND a.account_key=$2 AND j.entry_type_id=$3 AND j.order_id=$4", &[&f.corporation, &key, &REF_MARKET_TRANSACTION, &bid]).await.unwrap();
        assert_eq!(journal.get::<_, String>("amount"), "3943.98");
        seller.session.apply(vec![("corpid", Value::None), ("corpAccountKey", Value::None)]);
        test_db.execute("DELETE FROM game.market_orders WHERE order_id=$1", &[&bid]).await.unwrap();
        let before = f.fingerprint(&test_db).await;
        let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        seller.world.register_pilot(f.issuer.character, tx);
        assert_no_match(sell_rpc(&mut seller, vec![sell_line(f.at.id, f.type_id, item, 1971.99, 4, None)], false, 0).await);
        assert_eq!(f.fingerprint(&test_db).await, before);
        assert!(notices.try_recv().is_err());
    }).await;
    db.execute("DELETE FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![bid, wide]]).await.unwrap();
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn unmatched_and_resting_orders_keep_the_tick_rule() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let ask = orders::insert(&db, &npc_order(&db, f.at, f.type_id, "1972.00", false).await).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _peer) = client(test_db.clone(), f.issuer.character).await;
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.issuer.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let before = f.fingerprint(&test_db).await;
        let (tx, mut notices) = tokio::sync::mpsc::unbounded_channel();
        buyer.world.register_pilot(f.issuer.character, tx);
        assert_no_match(place_buy_rpc(&mut buyer, f.at.id, f.type_id, 1971.99, 1, 0, false).await);
        assert_eq!(f.fingerprint(&test_db).await, before);
        assert!(notices.try_recv().is_err());
        assert_invalid_price(place_buy_rpc(&mut buyer, f.at.id, f.type_id, 1971.99, 1, 1, false).await);
        assert_eq!(f.fingerprint(&test_db).await, before);
        assert_invalid_price(sell_rpc(&mut buyer, vec![sell_line(f.at.id, f.type_id, item, 1971.99, 1, None)], false, 1).await);
        assert_eq!(f.fingerprint(&test_db).await, before);
        assert!(place_buy_rpc(&mut buyer, f.at.id, f.type_id, 1971.00, 1, 1, false).await.is_ok());
        let order: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND bid", &[&f.issuer.character]).await.unwrap().get("order_id");
        assert_eq!(test_db.query_one("SELECT price::text AS price FROM game.market_orders WHERE order_id=$1", &[&order]).await.unwrap().get::<_, String>("price"), "1971.00");
        for price in [1971.99, 1971.50] {
            assert_invalid_price(Err(super::super::lifecycle::modify_order(&crate::services::identity::Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, modify_request(&buyer, order, price).await).await.unwrap_err()));
            assert_eq!(test_db.query_one("SELECT price::text AS price FROM game.market_orders WHERE order_id=$1", &[&order]).await.unwrap().get::<_, String>("price"), "1971.00");
        }
    }).await;
    db.execute("DELETE FROM game.market_orders WHERE order_id=$1", &[&ask]).await.unwrap();
    f.cleanup(&db).await;
    scenario.unwrap();
}
