//! Original corporate wallet ownership across order changes.
use super::*;
use super::corporate_fixture::CorporateFixture;
use super::super::{lifecycle, trading::place_buy};
use crate::services::{identity::Identity, tests::client};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_bid_reprice_and_cancel_refund_original_division() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.issuer.character).await;
        let key = ACCOUNT_CASH + 1;
        c.session.apply(vec![("corpid", Value::Int(f.corporation)), ("corpAccountKey", Value::Int(key.into())), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let order = place_buy(&Identity::of_client(&c), &c.db, &c.world, &c.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, Some(key)).await.unwrap();
        c.session.apply(vec![("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH)))]);
        crate::dao::market::tests::age_order_for_modification(&test_db, order.id).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, order.id, 120000.0).await).await.unwrap();
        crate::dao::market::tests::age_order_for_modification(&test_db, order.id).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, modify_request(&c, order.id, 110000.0).await).await.unwrap();
        lifecycle::cancel_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, order.id, f.at.region).await.unwrap();
        assert!(lifecycle::cancel_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, order.id, f.at.region).await.is_err());
        let rows = test_db.query("SELECT j.actor_id,j.owner_id1,j.owner_id2,j.reference_id,j.amount::text AS amount,a.account_key FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE j.order_id=$1 AND j.entry_type_id=$2 ORDER BY j.journal_id", &[&order.id, &REF_MARKET_ESCROW]).await.unwrap();
        assert_eq!(rows.len(), 4);
        for (row, amount) in rows.iter().zip(["-100000.00", "-20000.00", "10000.00", "110000.00"]) {
            assert_eq!(row.get::<_,i64>("actor_id"), f.issuer.character);
            assert_eq!(row.get::<_,i64>("owner_id1"), f.issuer.character);
            assert_eq!(row.get::<_,i64>("owner_id2"), OWNER_SCC);
            assert_eq!(row.get::<_,i64>("reference_id"), -1);
            assert_eq!(row.get::<_,i32>("account_key"), key);
            assert_eq!(row.get::<_,String>("amount"), amount);
        }
        let fees = test_db.query("SELECT j.owner_id1,j.actor_id,a.account_key FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE j.order_id=$1 AND j.entry_type_id=$2", &[&order.id, &REF_BROKER_FEE]).await.unwrap();
        assert_eq!(fees.len(), 3);
        for row in fees { assert_eq!(row.get::<_,i64>("owner_id1"), f.issuer.character); assert_eq!(row.get::<_,i64>("actor_id"), f.issuer.character); assert_eq!(row.get::<_,i32>("account_key"), key); }
        let conservation = test_db.query_one("SELECT (w.balance-10000000-sum(j.amount))::text AS difference FROM game.corp_wallets w JOIN game.wallet_accounts a ON a.owner_id=w.corporation_id AND a.account_key=w.account_key JOIN game.wallet_journal j USING(account_id) WHERE w.corporation_id=$1 AND w.account_key=$2 GROUP BY w.balance", &[&f.corporation, &key]).await.unwrap();
        assert_eq!(conservation.get::<_,String>("difference"), "0.00");
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn cancelled_corporate_ask_returns_to_deliveries_when_office_is_lost() {
    use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1};
    use super::super::trading::place_sell;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.issuer.character).await;
        c.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&c), &c.db, &c.world, &c.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 2, duration: 1, office_id: Some(f.office), expected_brokers_fee: None }, Some(ACCOUNT_CASH + 1)).await.unwrap();
        let order: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.issuer.character]).await.unwrap().get("order_id");
        test_db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&f.office]).await.unwrap();
        lifecycle::cancel_order(&Identity::of_client(&c), &c.db, &c.world, &c.sde, order, f.at.region).await.unwrap();
        let row = test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.corporation, &f.at.id, &FLAG_CORP_DELIVERIES, &f.type_id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("quantity"), 2);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_bid_cross_reprice_refunds_improvement_and_expiry_once() {
    use super::super::trading::place_sell;
    use infinity_core::consts::FLAG_HANGAR;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _b) = client(test_db.clone(), f.counterparty.character).await;
        buyer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        seller.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let key = ACCOUNT_CASH + 1;
        let bid = place_buy(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 90000.0, quantity: 2, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, Some(key)).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        buyer.session.apply(vec![("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH)))]);
        crate::dao::market::tests::age_order_for_modification(&test_db, bid.id).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, modify_request(&buyer, bid.id, 110000.0).await).await.unwrap();
        let open = test_db.query_one("SELECT vol_remaining,escrow::text AS escrow FROM game.market_orders WHERE order_id=$1", &[&bid.id]).await.unwrap();
        assert_eq!(open.get::<_, i64>("vol_remaining"), 1);
        assert_eq!(open.get::<_, String>("escrow"), "110000.00");
        test_db.execute("UPDATE game.market_orders SET issued=$2,expires_at=$2 WHERE order_id=$1", &[&bid.id, &(filetime_now() - 2 * FILETIME_DAY)]).await.unwrap();
        for _ in 0..2 {
            lifecycle::expire_orders(test_db.clone(), Arc::clone(&buyer.world), crate::scheduler::Job { id: "market-test".into(), kind: crate::scheduler::MARKET.into(), due_at: 0, payload: "{}".into() }).await.unwrap();
        }
        let row = test_db.query_one("SELECT (SELECT count(*) FROM game.market_orders WHERE order_id=$1) AS open, (SELECT count(*) FROM game.market_order_history WHERE order_id=$1 AND state=$2 AND corporation_id=$3 AND char_id=$4 AND wallet_key=$5) AS closed, (SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE j.order_id=$1 AND j.entry_type_id=$6 AND j.amount>0 AND a.owner_id=$3 AND a.account_key=$5) AS refunds, (SELECT COALESCE(sum(j.amount),0)::text FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE j.order_id=$1 AND j.entry_type_id=$6 AND a.owner_id=$3 AND a.account_key=$5) AS net", &[&bid.id, &STATE_CLOSED, &f.corporation, &f.issuer.character, &key, &REF_MARKET_ESCROW]).await.unwrap();
        assert_eq!(row.get::<_, i64>("open"), 0);
        assert_eq!(row.get::<_, i64>("closed"), 1);
        assert_eq!(row.get::<_, i64>("refunds"), 2);
        assert_eq!(row.get::<_, String>("net"), "-100000.00");
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn fractional_corporate_crossed_reprice_settles_once() {
    use super::super::trading::place_sell;
    use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_HANGAR, REF_TRANSACTION_TAX, SKILL_ACCOUNTING};
    crate::tests::init_config();
    assert_eq!(crate::config::game().market.broker_fee_min_isk, 100.0);
    assert_eq!(crate::config::game().market.sales_tax, 0.08);
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _b) = client(test_db.clone(), f.counterparty.character).await;
        buyer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        seller.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        crate::dao::skills::training::set_level(&test_db, f.counterparty.character, SKILL_ACCOUNTING, 0, 0).await.unwrap();
        let tax = super::super::fees::skills(&test_db, &seller.sde, f.counterparty.character, filetime_now()).await.unwrap().sales_tax;
        assert_eq!(tax, FeeRate::from_input(0.08).unwrap());
        let key = ACCOUNT_CASH + 1;
        let bid = place_buy(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 10.00, quantity: 3, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, Some(key)).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,3,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 10.01, quantity: 3, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let ask: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.counterparty.character]).await.unwrap().get("order_id");
        let resting = test_db.query_one("SELECT escrow::text AS escrow,vol_remaining FROM game.market_orders WHERE order_id=$1", &[&bid.id]).await.unwrap();
        assert_eq!(resting.get::<_, String>("escrow"), "30.00");
        assert_eq!(resting.get::<_, i64>("vol_remaining"), 3);
        let before = test_db.query_one("SELECT (SELECT balance::text FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2) AS corp, (SELECT balance::text FROM game.characters WHERE character_id=$3) AS seller, (SELECT count(*) FROM game.market_transactions WHERE buyer_id=$4 AND seller_id=$3) AS fills", &[&f.corporation, &key, &f.counterparty.character, &f.issuer.character]).await.unwrap();
        assert_eq!(before.get::<_, i64>("fills"), 0);
        let corp_before: String = before.get("corp");
        let seller_before: String = before.get("seller");
        buyer.session.apply(vec![("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH)))]);
        crate::dao::market::tests::age_order_for_modification(&test_db, bid.id).await.unwrap();
        lifecycle::modify_order(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, modify_request(&buyer, bid.id, 10.02).await).await.unwrap();
        let delta = test_db.query_one("SELECT (w.balance-$3::text::numeric)::text AS corp, (c.balance-$4::text::numeric)::text AS seller FROM game.corp_wallets w, game.characters c WHERE w.corporation_id=$1 AND w.account_key=$2 AND c.character_id=$5", &[&f.corporation, &key, &corp_before, &seller_before, &f.counterparty.character]).await.unwrap();
        assert_eq!(delta.get::<_, String>("corp"), "-100.03");
        assert_eq!(delta.get::<_, String>("seller"), "27.63");
        let closed = test_db.query_one("SELECT (SELECT count(*) FROM game.market_orders WHERE order_id=ANY($1)) AS open, (SELECT count(*) FROM game.market_order_history WHERE order_id=$2 AND corporation_id=$3 AND wallet_key=$4 AND state=$5 AND vol_remaining=0) AS bid_closed", &[&vec![bid.id, ask], &bid.id, &f.corporation, &key, &STATE_CLOSED]).await.unwrap();
        assert_eq!(closed.get::<_, i64>("open"), 0);
        assert_eq!(closed.get::<_, i64>("bid_closed"), 1);
        let fills = test_db.query("SELECT quantity,price::text AS price,buyer_account_id FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$2", &[&f.issuer.character, &f.counterparty.character]).await.unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].get::<_, i64>("quantity"), 3);
        assert_eq!(fills[0].get::<_, String>("price"), "10.01");
        let account: i64 = test_db.query_one("SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get("account_id");
        assert_eq!(fills[0].get::<_, Option<i64>>("buyer_account_id"), Some(account));
        let history = crate::dao::market::listings::price_history(&test_db, f.at.region, &[f.type_id], FILETIME_DAY, 0).await.unwrap();
        assert_eq!(history.iter().map(|h| h.volume).sum::<i128>(), 3);
        let delivery = test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.corporation, &f.at.id, &FLAG_CORP_DELIVERIES, &f.type_id]).await.unwrap();
        assert_eq!(delivery.get::<_, i64>("quantity"), 3);
        let escrow = test_db.query("SELECT j.amount::text AS amount FROM game.wallet_journal j WHERE j.account_id=$1 AND j.order_id=$2 AND j.entry_type_id=$3 ORDER BY j.journal_id", &[&account, &bid.id, &REF_MARKET_ESCROW]).await.unwrap();
        assert_eq!(escrow.iter().map(|r| r.get::<_, String>("amount")).collect::<Vec<_>>(), ["-30.00", "-0.06", "0.03"]);
        let journals = test_db.query_one("SELECT count(*) FILTER (WHERE j.account_id=$1 AND j.entry_type_id=$2) AS buyer_trade, COALESCE(sum(j.amount) FILTER (WHERE a.owner_id=$3 AND j.entry_type_id=$2),0)::text AS gross, COALESCE(sum(j.amount) FILTER (WHERE a.owner_id=$3 AND j.entry_type_id=$4),0)::text AS tax FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE j.market_transaction_id=(SELECT transaction_id FROM game.market_transactions WHERE buyer_id=$5 AND seller_id=$3)", &[&account, &REF_MARKET_TRANSACTION, &f.counterparty.character, &REF_TRANSACTION_TAX, &f.issuer.character]).await.unwrap();
        assert_eq!(journals.get::<_, i64>("buyer_trade"), 0);
        assert_eq!(journals.get::<_, String>("gross"), "30.03");
        assert_eq!(journals.get::<_, String>("tax"), "-2.40");
        let fees = test_db.query("SELECT amount::text AS amount FROM game.wallet_journal WHERE account_id=$1 AND order_id=$2 AND entry_type_id=$3 ORDER BY journal_id", &[&account, &bid.id, &REF_BROKER_FEE]).await.unwrap();
        assert_eq!(fees.iter().map(|r| r.get::<_, String>("amount")).collect::<Vec<_>>(), ["-100.00", "-100.00"]);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}
