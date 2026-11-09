//! Corporate market settlement against personal counterparties.
use super::*;
use super::corporate_fixture::CorporateFixture;
use super::super::trading::{place_buy, place_sell};
use infinity_core::consts::{FLAG_CORP_SAG1, FLAG_CORP_DELIVERIES, FLAG_HANGAR, REF_TRANSACTION_TAX};
use crate::services::{identity::Identity, tests::client};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn personal_seller_fills_corporate_escrow_without_second_debit() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut corp, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _b) = client(test_db.clone(), f.counterparty.character).await;
        corp.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        seller.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let key = ACCOUNT_CASH + 1;
        let order = place_buy(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, Some(key)).await.unwrap();
        let before = test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get::<_, String>("balance");
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 0, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let after = test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get::<_, String>("balance");
        assert_eq!(before, after);
        let fill = test_db.query_one("SELECT buyer_id,seller_id,buyer_account_id FROM game.market_transactions WHERE order_id=$1", &[&order.id]).await.unwrap();
        assert_eq!(fill.get::<_,i64>("buyer_id"), f.issuer.character);
        assert_eq!(fill.get::<_,i64>("seller_id"), f.counterparty.character);
        let account: i64 = test_db.query_one("SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get("account_id");
        assert_eq!(fill.get::<_,Option<i64>>("buyer_account_id"), Some(account));
        let row = test_db.query_one("SELECT owner_id1,owner_id2 FROM game.wallet_journal WHERE order_id=$1 AND entry_type_id=$2", &[&order.id, &REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(row.get::<_,i64>("owner_id1"), f.corporation);
        assert_eq!(row.get::<_,i64>("owner_id2"), f.counterparty.character);
        let delivery = test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.corporation, &f.at.id, &FLAG_CORP_DELIVERIES, &f.type_id]).await.unwrap();
        assert_eq!(delivery.get::<_, i64>("quantity"), 1);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn personal_buyer_lifts_corporate_office_ask() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut corp, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut buyer, _b) = client(test_db.clone(), f.counterparty.character).await;
        corp.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let key = ACCOUNT_CASH + 1;
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,3,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 2, duration: 1, office_id: Some(f.office), expected_brokers_fee: None }, Some(key)).await.unwrap();
        let ask = test_db.query_one("SELECT corporation_id,wallet_key,vol_remaining FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.issuer.character]).await.unwrap();
        assert_eq!(ask.get::<_,Option<i64>>("corporation_id"), Some(f.corporation));
        assert_eq!(ask.get::<_,i32>("wallet_key"), key);
        assert_eq!(ask.get::<_, i64>("vol_remaining"), 2);
        place_buy(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i64>("quantity"), 1);
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.counterparty.character, &f.at.id, &FLAG_HANGAR, &f.type_id]).await.unwrap().get::<_, i64>("quantity"), 1);
        let fee = test_db.query_one("SELECT j.actor_id,j.owner_id1,a.account_key FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$2", &[&f.corporation, &REF_BROKER_FEE]).await.unwrap();
        assert_eq!(fee.get::<_,i64>("actor_id"), f.issuer.character);
        assert_eq!(fee.get::<_,i64>("owner_id1"), f.issuer.character);
        assert_eq!(fee.get::<_,i32>("account_key"), key);
        let trade = test_db.query_one("SELECT owner_id1,owner_id2 FROM game.wallet_journal WHERE actor_id=$1 AND entry_type_id=$2", &[&f.issuer.character, &REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(trade.get::<_,i64>("owner_id1"), f.counterparty.character);
        assert_eq!(trade.get::<_,i64>("owner_id2"), f.corporation);
        let conservation = test_db.query_one("SELECT (w.balance-10000000-sum(j.amount))::text AS difference FROM game.corp_wallets w JOIN game.wallet_accounts a ON a.owner_id=w.corporation_id AND a.account_key=w.account_key JOIN game.wallet_journal j USING(account_id) WHERE w.corporation_id=$1 AND w.account_key=$2 GROUP BY w.balance", &[&f.corporation, &key]).await.unwrap();
        assert_eq!(conservation.get::<_,String>("difference"), "0.00");
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_order_passive_fill_survives_issuer_departure() {
    use super::super::lifecycle;
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut issuer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut buyer, _b) = client(test_db.clone(), f.counterparty.character).await;
        issuer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let key = ACCOUNT_CASH + 1;
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&issuer), &issuer.db, &issuer.world, &issuer.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: Some(f.office), expected_brokers_fee: None }, Some(key)).await.unwrap();
        let id: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.issuer.character]).await.unwrap().get("order_id");
        fixture::assign_corporation(&test_db, f.issuer.character, 1, 0).await.unwrap();
        issuer.session.apply(vec![("corpid", Value::Int(1))]);
        assert!(lifecycle::modify_order(&Identity::of_client(&issuer), &issuer.db, &issuer.world, &issuer.sde, modify_request(&issuer, id, 110000.0).await).await.is_err());
        assert!(lifecycle::cancel_order(&Identity::of_client(&issuer), &issuer.db, &issuer.world, &issuer.sde, id, f.at.region).await.is_err());
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get("balance");
        place_buy(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        let row = test_db.query_one("SELECT (w.balance-$3::text::numeric-sum(j.amount))::text AS difference, count(*) FILTER (WHERE j.entry_type_id=$4) AS gross, count(*) FILTER (WHERE j.entry_type_id=$5) AS tax FROM game.corp_wallets w JOIN game.wallet_accounts a ON a.owner_id=w.corporation_id AND a.account_key=w.account_key JOIN game.wallet_journal j USING(account_id) WHERE w.corporation_id=$1 AND w.account_key=$2 AND j.order_id=$6 AND j.entry_type_id IN ($4,$5) GROUP BY w.balance", &[&f.corporation, &key, &before, &REF_MARKET_TRANSACTION, &REF_TRANSACTION_TAX, &id]).await.unwrap();
        assert_eq!(row.get::<_, String>("difference"), "0.00");
        assert_eq!(row.get::<_, i64>("gross"), 1);
        assert_eq!(row.get::<_, i64>("tax"), 1);
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i64>("quantity"), 1);
        let history = test_db.query_one("SELECT char_id,corporation_id,wallet_key FROM game.market_order_history WHERE order_id=$1", &[&id]).await.unwrap();
        assert_eq!(history.get::<_, i64>("char_id"), f.issuer.character);
        assert_eq!(history.get::<_, Option<i64>>("corporation_id"), Some(f.corporation));
        assert_eq!(history.get::<_, i32>("wallet_key"), key);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_trade_with_npc_orders_preserves_unlimited_no_wallet_side() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let mut npc = Order { id: orders::next_order_id(&db).await.unwrap(), type_id: f.type_id, char_id: None, corporation_id: None, wallet_key: ACCOUNT_CASH, source_location_id: None, source_flag_id: None, at: f.at, price: crate::isk::IskAmount::from_whole_isk(100000).unwrap(), vol_entered: 1, vol_remaining: 1, min_volume: 1, bid: false, range: RANGE_STATION, duration: 1, issued: None, escrow: crate::isk::IskAmount::ZERO, scope: orders::MarketScope::for_type(f.type_id), vault_user_id: None, plex_escrow: 0, created_at: None, expires_at: None };
    let ask = orders::insert(&db, &npc).await.unwrap();
    npc.id = orders::next_order_id(&db).await.unwrap();
    npc.bid = true;
    let bid = orders::insert(&db, &npc).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut corp, _peer) = client(test_db.clone(), f.issuer.character).await;
        corp.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let key = ACCOUNT_CASH + 1;
        place_buy(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 2, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, Some(key)).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 2, duration: 0, office_id: Some(f.office), expected_brokers_fee: None }, Some(key)).await.unwrap();
        let rows = test_db.query("SELECT order_id,buyer_account_id,seller_account_id,quantity FROM game.market_transactions WHERE order_id=ANY($1) ORDER BY order_id", &[&vec![ask, bid]]).await.unwrap();
        assert_eq!(rows.len(), 2);
        let account: i64 = test_db.query_one("SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get("account_id");
        for row in rows {
            let is_ask = row.get::<_, i64>("order_id") == ask;
            assert_eq!(row.get::<_, i64>("quantity"), 2);
            assert_eq!(row.get::<_, Option<i64>>("buyer_account_id"), is_ask.then_some(account));
            assert_eq!(row.get::<_, Option<i64>>("seller_account_id"), (!is_ask).then_some(account));
        }
        let rows = test_db.query("SELECT vol_remaining FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask, bid]]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.get::<_, i64>("vol_remaining") == 1));
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.corporation, &f.at.id, &FLAG_CORP_DELIVERIES, &f.type_id]).await.unwrap().get::<_, i64>("quantity"), 2);
        let journal = test_db.query_one("SELECT count(*) FILTER (WHERE j.entry_type_id=$3) AS trades, count(*) FILTER (WHERE j.entry_type_id=$4) AS taxes FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND a.account_key=$2 AND j.order_id=ANY($5)", &[&f.corporation, &key, &REF_MARKET_TRANSACTION, &REF_TRANSACTION_TAX, &vec![ask, bid]]).await.unwrap();
        assert_eq!(journal.get::<_, i64>("trades"), 2);
        assert_eq!(journal.get::<_, i64>("taxes"), 1);
    }).await;
    db.execute("DELETE FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask, bid]]).await.unwrap();
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn large_corporate_sale_journal_components_equal_exact_wallet_change() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut corp, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut buyer, _b) = client(test_db.clone(), f.counterparty.character).await;
        corp.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let key = ACCOUNT_CASH + 1;
        test_db.execute("UPDATE game.corp_wallets SET balance=100000000000000000000000000000 WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap();
        test_db.execute("UPDATE game.characters SET balance=100000000000000000000000000000 WHERE character_id=$1", &[&f.counterparty.character]).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1, &i64::from(i32::MAX)]).await.unwrap().get("item_id");
        let price = 9_223_000_000_000.0;
        place_sell(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price, quantity: i64::from(i32::MAX), duration: 1, office_id: Some(f.office), expected_brokers_fee: None }, Some(key)).await.unwrap();
        let id: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.issuer.character]).await.unwrap().get("order_id");
        let gross = crate::isk::IskAmount::from_whole_isk(9_223_000_000_000).unwrap().checked_mul(i64::from(i32::MAX)).unwrap();
        let rate = super::super::fees::skills(&test_db, &corp.sde, f.issuer.character, crate::client::filetime_now()).await.unwrap().sales_tax;
        let tax = super::super::fees::apply_rate(gross, rate).unwrap();
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get("balance");
        place_buy(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price, quantity: i64::from(i32::MAX), range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        let sale = test_db.query_one("SELECT (w.balance-$3::text::numeric-sum(j.amount))::text AS difference, count(*) FILTER (WHERE j.entry_type_id=$4) AS gross_count, count(*) FILTER (WHERE j.entry_type_id=$5) AS tax_count, sum(j.amount) FILTER (WHERE j.entry_type_id=$4)::text AS gross, sum(j.amount) FILTER (WHERE j.entry_type_id=$5)::text AS tax, (w.balance-$3::text::numeric)::text AS net FROM game.corp_wallets w JOIN game.wallet_accounts a ON a.owner_id=w.corporation_id AND a.account_key=w.account_key JOIN game.wallet_journal j USING(account_id) WHERE w.corporation_id=$1 AND w.account_key=$2 AND j.order_id=$6 AND j.entry_type_id IN ($4,$5) GROUP BY w.balance", &[&f.corporation, &key, &before, &REF_MARKET_TRANSACTION, &REF_TRANSACTION_TAX, &id]).await.unwrap();
        assert_eq!(sale.get::<_, String>("difference"), "0.00");
        assert_eq!(sale.get::<_, i64>("gross_count"), 1);
        assert_eq!(sale.get::<_, i64>("tax_count"), 1);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&sale.get::<_, String>("gross")).unwrap(), gross);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&sale.get::<_, String>("tax")).unwrap(), tax.checked_neg().unwrap());
        assert_eq!(crate::isk::IskAmount::parse_decimal(&sale.get::<_, String>("net")).unwrap(), gross.checked_sub(tax).unwrap());
        let debit = test_db.query_one("SELECT count(*) AS n, (c.balance-100000000000000000000000000000-sum(j.amount))::text AS difference FROM game.characters c JOIN game.wallet_accounts a ON a.owner_id=c.character_id JOIN game.wallet_journal j USING(account_id) WHERE c.character_id=$1 AND j.order_id=$2 AND j.entry_type_id=$3 GROUP BY c.balance", &[&f.counterparty.character, &id, &REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(debit.get::<_, i64>("n"), 1);
        assert_eq!(debit.get::<_, String>("difference"), "0.00");
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn same_account_is_skipped_but_cross_division_projects_both_sides() {
    use super::super::authorization;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut corp, _peer) = client(test_db.clone(), f.issuer.character).await;
        corp.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let seller_key = ACCOUNT_CASH + 1;
        let buyer_key = ACCOUNT_CASH;
        test_db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,10000000) ON CONFLICT (corporation_id,account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&f.corporation, &buyer_key]).await.unwrap();
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.corporation, &f.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: Some(f.office), expected_brokers_fee: None }, Some(seller_key)).await.unwrap();
        let ask: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&f.issuer.character]).await.unwrap().get("order_id");
        let request = BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None };
        let before = test_db.query_one("SELECT balance::text AS balance,(SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1) AS journals FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &seller_key]).await.unwrap();
        assert!(place_buy(&Identity::of_client(&corp), &corp.db, &corp.world, &corp.sde, request, Some(seller_key)).await.is_err());
        crate::services::tests::fixture::assign_corporation(&test_db, f.counterparty.character, f.corporation, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        let (mut other, _other_peer) = client(test_db.clone(), f.counterparty.character).await;
        other.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        assert!(place_buy(&Identity::of_client(&other), &other.db, &other.world, &other.sde, request, Some(seller_key)).await.is_err());
        let after = test_db.query_one("SELECT balance::text AS balance,(SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1) AS journals FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &seller_key]).await.unwrap();
        assert_eq!(before.get::<_, String>("balance"), after.get::<_, String>("balance"));
        assert_eq!(before.get::<_, i64>("journals"), after.get::<_, i64>("journals"));
        assert_eq!(test_db.query_one("SELECT count(*) AS n FROM game.market_transactions WHERE order_id=$1", &[&ask]).await.unwrap().get::<_, i64>("n"), 0);
        let identity = Identity::of_client(&corp);
        assert!(authorization::readable_divisions(&test_db, &identity, Some(seller_key)).await.is_ok());
        let keys = authorization::readable_divisions(&test_db, &identity, None).await.unwrap();
        place_buy(&identity, &corp.db, &corp.world, &corp.sde, request, Some(buyer_key)).await.unwrap();
        let sides = listings::corporation_transactions(&test_db, f.corporation, &keys, 0).await.unwrap();
        assert_eq!(sides.len(), 2);
        assert_eq!(sides[0].transaction_id, sides[1].transaction_id);
        assert!(sides.iter().all(|r| r.buyer_id == f.issuer.character && r.seller_id == f.issuer.character));
        assert!(sides.iter().any(|r| r.account_id == r.buyer_account_id));
        assert!(sides.iter().any(|r| r.account_id == r.seller_account_id));
        assert_ne!(sides[0].account_id, sides[1].account_id);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&listings::escrow(&test_db, f.issuer.character).await.unwrap().items).unwrap(), crate::isk::IskAmount::ZERO);
        let history = test_db.query_one("SELECT char_id,corporation_id,wallet_key FROM game.market_order_history WHERE order_id=$1", &[&ask]).await.unwrap();
        assert_eq!(history.get::<_, i64>("char_id"), f.issuer.character);
        assert_eq!(history.get::<_, Option<i64>>("corporation_id"), Some(f.corporation));
        assert_eq!(history.get::<_, i32>("wallet_key"), seller_key);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_immediate_purchase_uses_no_office_fill_station_and_merged_receipt() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let member = fixture::create(&db).await.unwrap();
    let second_viewer = fixture::create(&db).await.unwrap();
    let departed = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        fixture::assign_corporation(&test_db, member.character, f.corporation, 0).await.unwrap();
        fixture::assign_corporation(&test_db, second_viewer.character, f.corporation, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        fixture::assign_corporation(&test_db, departed.character, f.corporation, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        fixture::assign_corporation(&test_db, departed.character, 1, 0).await.unwrap();
        let (mut buyer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _b) = client(test_db.clone(), f.counterparty.character).await;
        let remote = market_station(buyer.sde.map.stations().find(|s| s.station_id != f.at.id && s.solar_system_id == Some(f.at.system) && s.constellation_id.is_some() && s.region_id == Some(f.at.region) && s.owner_id.is_some()).expect("another NPC station in fixture system"));
        buyer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        seller.session.apply(vec![("stationid", Value::Int(remote.id)), ("solarsystemid2", Value::Int(remote.system))]);
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &remote.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: remote.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let merged: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,3,false) RETURNING item_id", &[&f.type_id, &f.corporation, &remote.id, &FLAG_CORP_DELIVERIES]).await.unwrap().get("item_id");
        let (viewer_tx, mut viewer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (member_tx, mut member_rx) = tokio::sync::mpsc::unbounded_channel();
        let (foreign_tx, mut foreign_rx) = tokio::sync::mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
        let (departed_tx, mut departed_rx) = tokio::sync::mpsc::unbounded_channel();
        buyer.world.register_pilot(f.issuer.character, viewer_tx);
        buyer.world.register_pilot(member.character, member_tx);
        buyer.world.register_pilot(f.counterparty.character, foreign_tx);
        buyer.world.register_pilot(second_viewer.character, second_tx);
        buyer.world.register_pilot(departed.character, departed_tx);
        let key = ACCOUNT_CASH + 1;
        let issuer_before: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get("balance");
        place_buy(&Identity::of_client(&buyer), &buyer.db, &buyer.world, &buyer.sde, BuyRequest { station_id: remote.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, Some(key)).await.unwrap();
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&merged]).await.unwrap().get::<_, i64>("quantity"), 4);
        assert_eq!(test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&f.corporation, &key]).await.unwrap().get::<_, String>("balance"), "9900000.00");
        assert_eq!(test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get::<_, String>("balance"), issuer_before);
        let receipt = std::iter::from_fn(|| viewer_rx.try_recv().ok()).find(|(name, _)| name == "OnMarketItemReceived").expect("director receives merged delivery");
        assert_eq!(receipt.1, vec![Value::Int(merged), Value::Int(i64::from(f.type_id)), Value::Int(i64::from(FLAG_CORP_DELIVERIES)), Value::Int(remote.id)]);
        let second_receipts: Vec<_> = std::iter::from_fn(|| second_rx.try_recv().ok()).filter(|(name, _)| name == "OnMarketItemReceived").collect();
        assert_eq!(second_receipts.len(), 1);
        assert_eq!(second_receipts[0].1, receipt.1);
        assert!(!std::iter::from_fn(|| departed_rx.try_recv().ok()).any(|(name, _)| name == "OnMarketItemReceived"));
        assert!(!std::iter::from_fn(|| member_rx.try_recv().ok()).any(|(name, _)| name == "OnMarketItemReceived"));
        assert!(!std::iter::from_fn(|| foreign_rx.try_recv().ok()).any(|(name, _)| name == "OnMarketItemReceived"));
    }).await;
    fixture::assign_corporation(&db, member.character, 1, 0).await.unwrap();
    fixture::cleanup(&db, member).await.unwrap();
    fixture::assign_corporation(&db, second_viewer.character, 1, 0).await.unwrap();
    fixture::cleanup(&db, second_viewer).await.unwrap();
    fixture::cleanup(&db, departed).await.unwrap();
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn distinct_corporations_fill_offline_issuer_order_at_current_tax() {
    use crate::services::{identity::Identity, tests::fixture};
    use super::super::{fees, lifecycle, market_proxy};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let seller = CorporateFixture::create(&db).await;
    let buyer = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let key = ACCOUNT_CASH + 1;
        fixture::assign_corporation(&test_db, seller.counterparty.character, seller.corporation, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        let (mut issuer, _issuer_peer) = client(test_db.clone(), seller.issuer.character).await;
        let (mut seller_viewer, _viewer_peer) = client(test_db.clone(), seller.counterparty.character).await;
        let (mut purchaser, _buyer_peer) = client(test_db.clone(), buyer.issuer.character).await;
        for (c, corp) in [(&mut issuer, seller.corporation), (&mut seller_viewer, seller.corporation), (&mut purchaser, buyer.corporation)] {
            c.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(seller.at.id)), ("solarsystemid2", Value::Int(seller.at.system)), ("regionid", Value::Int(seller.at.region))]);
        }
        let item: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&seller.type_id, &seller.corporation, &seller.office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
        crate::dao::skills::training::set_level(&test_db, seller.issuer.character, infinity_core::consts::SKILL_ACCOUNTING, 0, 0).await.unwrap();
        let initial_tax = fees::skills(&test_db, &issuer.sde, seller.issuer.character, crate::client::filetime_now()).await.unwrap().sales_tax;
        place_sell(&Identity::of_client(&issuer), &test_db, &issuer.world, &issuer.sde, SellRequest { item_id: item, station_id: seller.at.id, type_id: seller.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: Some(seller.office), expected_brokers_fee: None }, Some(key)).await.unwrap();
        let order: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND NOT bid", &[&seller.issuer.character]).await.unwrap().get("order_id");
        fixture::assign_corporation(&test_db, seller.issuer.character, 1, 0).await.unwrap();
        issuer.session.apply(vec![("corpid", Value::Int(1))]);
        assert!(lifecycle::cancel_order(&Identity::of_client(&issuer), &test_db, &issuer.world, &issuer.sde, order, seller.at.region).await.is_err());
        let rank = crate::services::skills::training::skill_type(&issuer.sde, infinity_core::consts::SKILL_ACCOUNTING).rank;
        let points = crate::services::skills::formulas::sp_for_level(rank, 5);
        crate::dao::skills::training::set_level(&test_db, seller.issuer.character, infinity_core::consts::SKILL_ACCOUNTING, 5, points).await.unwrap();
        let current_tax = fees::skills(&test_db, &issuer.sde, seller.issuer.character, crate::client::filetime_now()).await.unwrap().sales_tax;
        let gross = crate::isk::IskAmount::from_whole_isk(100000).unwrap();
        assert!(fees::apply_rate(gross, current_tax).unwrap() < fees::apply_rate(gross, initial_tax).unwrap());
        let own_cash = crate::dao::wallets::accounts::ensure(&test_db, seller.issuer.character, ACCOUNT_CASH).await.unwrap().id;
        drop(issuer);
        let personal: Vec<String> = test_db.query("SELECT balance::text AS balance FROM game.characters WHERE character_id=ANY($1) ORDER BY character_id", &[&vec![seller.issuer.character, buyer.issuer.character]]).await.unwrap().iter().map(|r| r.get("balance")).collect();
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&seller.corporation, &key]).await.unwrap().get("balance");
        place_buy(&Identity::of_client(&purchaser), &test_db, &purchaser.world, &purchaser.sde, BuyRequest { station_id: seller.at.id, type_id: seller.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, Some(key)).await.unwrap();
        assert_eq!(test_db.query_one("SELECT balance::text AS balance FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$2", &[&buyer.corporation, &key]).await.unwrap().get::<_, String>("balance"), "9900000.00");
        let unchanged: Vec<String> = test_db.query("SELECT balance::text AS balance FROM game.characters WHERE character_id=ANY($1) ORDER BY character_id", &[&vec![seller.issuer.character, buyer.issuer.character]]).await.unwrap().iter().map(|r| r.get("balance")).collect();
        assert_eq!(personal, unchanged);
        let fill = test_db.query_one("SELECT transaction_id,buyer_account_id,seller_account_id,quantity FROM game.market_transactions WHERE order_id=$1", &[&order]).await.unwrap();
        assert_eq!(fill.get::<_, i64>("quantity"), 1);
        let seller_account: i64 = fill.get("seller_account_id");
        let buyer_account: i64 = fill.get("buyer_account_id");
        let owners = test_db.query("SELECT account_id,owner_id,account_key FROM game.wallet_accounts WHERE account_id=ANY($1) ORDER BY owner_id", &[&vec![seller_account, buyer_account]]).await.unwrap();
        assert_eq!(owners.len(), 2);
        assert!(owners.iter().any(|r| r.get::<_, i64>("account_id") == seller_account && r.get::<_, i64>("owner_id") == seller.corporation && r.get::<_, i32>("account_key") == key));
        assert!(owners.iter().any(|r| r.get::<_, i64>("account_id") == buyer_account && r.get::<_, i64>("owner_id") == buyer.corporation && r.get::<_, i32>("account_key") == key));
        assert_ne!(seller_account, buyer_account);
        let tax = fees::apply_rate(gross, current_tax).unwrap();
        let row = test_db.query_one("SELECT (w.balance-$3::text::numeric-sum(j.amount))::text AS difference, sum(j.amount) FILTER (WHERE j.entry_type_id=$4)::text AS tax, sum(j.amount) FILTER (WHERE j.entry_type_id=$6)::text AS gross, (w.balance-$3::text::numeric)::text AS net FROM game.corp_wallets w JOIN game.wallet_accounts a ON a.owner_id=w.corporation_id AND a.account_key=w.account_key JOIN game.wallet_journal j USING(account_id) WHERE w.corporation_id=$1 AND w.account_key=$2 AND j.order_id=$5 AND j.entry_type_id IN ($4,$6) GROUP BY w.balance", &[&seller.corporation, &key, &before, &REF_TRANSACTION_TAX, &order, &REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("difference")).unwrap(), crate::isk::IskAmount::ZERO);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("tax")).unwrap(), tax.checked_neg().unwrap());
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("gross")).unwrap(), gross);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("net")).unwrap(), gross.checked_sub(tax).unwrap());
        for (viewer, method, account) in [(&mut seller_viewer, "CorpGetTransactions", seller_account), (&mut purchaser, "CorpGetTransactions", buyer_account)] {
            let args = [Value::Int(0), Value::Int(i64::from(key))];
            let kwargs = Value::None;
            let call = Call { service: "marketProxy", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            let wire = marshal::loads(&marshal::dumps(&market_proxy::dispatch(viewer, &call).await.unwrap().unwrap())).unwrap();
            let Value::Reduce { data, .. } = wire else { panic!("expected transaction CRowset") };
            let args = data.items().unwrap()[1].items().unwrap();
            let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
            let rows = args[1].items().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(descriptor.get(&rows[0], "accountID"), Some(Value::Int(account)));
            assert_eq!(descriptor.get(&rows[0], "transactionID"), Some(Value::Int(fill.get("transaction_id"))));
        }
        let (mut former_issuer, _former_peer) = client(test_db.clone(), seller.issuer.character).await;
        let args = [Value::Int(0)];
        let kwargs = Value::None;
        let call = Call { service: "marketProxy", method: "CharGetTransactions", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let wire = marshal::loads(&marshal::dumps(&market_proxy::dispatch(&mut former_issuer, &call).await.unwrap().unwrap())).unwrap();
        let Value::Reduce { data, .. } = wire else { panic!("expected transaction CRowset") };
        let args = data.items().unwrap()[1].items().unwrap();
        let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
        let rows = args[1].items().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(descriptor.get(&rows[0], "accountID"), Some(Value::Int(own_cash)));
        assert_eq!(descriptor.get(&rows[0], "sellerAccountID"), Some(Value::Int(seller_account)));
        assert_ne!(own_cash, seller_account);
        let history = listings::price_history(&test_db, seller.at.region, &[seller.type_id], FILETIME_DAY, 0).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!((history[0].volume, history[0].count), (1, 1));
    }).await;
    seller.cleanup(&db).await;
    buyer.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_plex_buys_cross_regions_and_deliver_at_fill_station() {
    use infinity_core::consts::TYPE_PLEX;
    use crate::dao::{accounts::plex, items::{loading::{self, ItemQuery}, transfers}, wallets::{accounts, characters, corporations}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut f = CorporateFixture::create(&db).await;
    f.type_id = TYPE_PLEX;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut corp, _) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _) = client(test_db.clone(), f.counterparty.character).await;
        let remote = market_station(seller.sde.map.stations().find(|s| s.region_id.is_some_and(|r| r != f.at.region) && s.solar_system_id.is_some() && s.constellation_id.is_some() && s.owner_id.is_some()).unwrap());
        corp.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        seller.session.apply(vec![("stationid", Value::Int(remote.id)), ("solarsystemid2", Value::Int(remote.system))]);
        let now = crate::client::filetime_now();
        assert!(listings::orders(&test_db, now, FILETIME_DAY, TYPE_PLEX, listings::Area::Region, f.at.region, None).await.unwrap().is_empty(), "dedicated database requires an empty global PLEX book");
        let key = ACCOUNT_CASH + 1;
        let vault = plex::balance(&test_db, f.issuer.account).await.unwrap();
        let personal = characters::balance(&test_db, f.issuer.character).await.unwrap();
        let account = accounts::ensure(&test_db, f.corporation, key).await.unwrap().id;
        let item = transfers::insert(&test_db, TYPE_PLEX, f.counterparty.character, remote.id, FLAG_HANGAR, 2).await.unwrap();
        place_sell(&Identity::of_client(&seller), &test_db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: remote.id, type_id: TYPE_PLEX.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        corp.world.register_pilot(f.issuer.character, tx);
        let before = corporations::balance(&test_db, f.corporation, key).await.unwrap();
        let buy = BuyRequest { station_id: f.at.id, type_id: TYPE_PLEX.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None };
        place_buy(&Identity::of_client(&corp), &test_db, &corp.world, &corp.sde, buy, Some(key)).await.unwrap();
        let gross = crate::isk::IskAmount::from_whole_isk(100000).unwrap();
        assert_eq!(corporations::balance(&test_db, f.corporation, key).await.unwrap(), before.checked_sub(gross).unwrap());
        let delivery = loading::load_items(&test_db, ItemQuery::OwnerLocationFlagTypeDescending(f.corporation, remote.id, FLAG_CORP_DELIVERIES, TYPE_PLEX)).await.unwrap();
        assert_eq!(delivery.iter().map(|i| i128::from(i.quantity)).sum::<i128>(), 1);
        assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|(name, args)| name == "OnMarketItemReceived" && args == vec![Value::Int(delivery[0].item_id), Value::Int(TYPE_PLEX.into()), Value::Int(FLAG_CORP_DELIVERIES.into()), Value::Int(remote.id)]));
        let standing = place_buy(&Identity::of_client(&corp), &test_db, &corp.world, &corp.sde, BuyRequest { duration: 1, ..buy }, Some(key)).await.unwrap();
        assert_eq!(standing.escrow, gross);
        let reserved = corporations::balance(&test_db, f.corporation, key).await.unwrap();
        place_sell(&Identity::of_client(&seller), &test_db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: remote.id, type_id: TYPE_PLEX.into(), price: 100000.0, quantity: 1, duration: 0, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        assert_eq!(corporations::balance(&test_db, f.corporation, key).await.unwrap(), reserved);
        assert_eq!(loading::load_items(&test_db, ItemQuery::OwnerLocationFlagTypeDescending(f.corporation, remote.id, FLAG_CORP_DELIVERIES, TYPE_PLEX)).await.unwrap().iter().map(|i| i128::from(i.quantity)).sum::<i128>(), 2);
        assert_eq!(plex::balance(&test_db, f.issuer.account).await.unwrap(), vault);
        assert_eq!(characters::balance(&test_db, f.issuer.character).await.unwrap(), personal);
        let fills = listings::corporation_transactions(&test_db, f.corporation, &[key], 0).await.unwrap();
        assert_eq!(fills.len(), 2);
        for fill in fills {
            assert_eq!((fill.buyer_id, fill.seller_id, fill.station_id, fill.quantity, fill.price, fill.buyer_account_id), (f.issuer.character, f.counterparty.character, remote.id, 1, gross, Some(account)));
            assert!(fill.seller_account_id.is_some());
        }
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_plex_sell_keeps_original_custody_after_departure() {
    use infinity_core::consts::TYPE_PLEX;
    use crate::dao::{accounts::plex, items::{loading::{self, ItemQuery}, transfers}, wallets::{accounts, characters}};
    use crate::services::tests::fixture;
    use super::super::lifecycle;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut f = CorporateFixture::create(&db).await;
    f.type_id = TYPE_PLEX;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut issuer, _) = client(test_db.clone(), f.issuer.character).await;
        let (mut buyer, _) = client(test_db.clone(), f.counterparty.character).await;
        let remote = market_station(buyer.sde.map.stations().find(|s| s.region_id.is_some_and(|r| r != f.at.region) && s.solar_system_id.is_some() && s.constellation_id.is_some() && s.owner_id.is_some()).unwrap());
        issuer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(remote.id)), ("solarsystemid2", Value::Int(remote.system))]);
        buyer.session.apply(vec![("stationid", Value::Int(remote.id)), ("solarsystemid2", Value::Int(remote.system))]);
        let now = crate::client::filetime_now();
        assert!(listings::orders(&test_db, now, FILETIME_DAY, TYPE_PLEX, listings::Area::Region, f.at.region, None).await.unwrap().is_empty());
        let key = ACCOUNT_CASH + 1;
        let item = transfers::insert(&test_db, TYPE_PLEX, f.corporation, f.office, FLAG_CORP_SAG1, 3).await.unwrap();
        let vault = plex::balance(&test_db, f.issuer.account).await.unwrap();
        let personal = characters::balance(&test_db, f.issuer.character).await.unwrap();
        let buyer_vault = plex::balance(&test_db, f.counterparty.account).await.unwrap();
        place_sell(&Identity::of_client(&issuer), &test_db, &issuer.world, &issuer.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: TYPE_PLEX.into(), price: 100000.0, quantity: 2, duration: 1, office_id: Some(f.office), expected_brokers_fee: None }, Some(key)).await.unwrap();
        let order = orders::my_orders(&test_db, f.issuer.character).await.unwrap().pop().unwrap();
        assert_eq!((order.plex_escrow, order.vol_remaining, order.source_location_id, order.source_flag_id, order.vault_user_id), (2, 2, Some(f.office), Some(FLAG_CORP_SAG1), None));
        assert_eq!(loading::load_items(&test_db, ItemQuery::Id(item)).await.unwrap()[0].quantity, 1);
        fixture::assign_corporation(&test_db, f.issuer.character, 1, 0).await.unwrap();
        issuer.session.apply(vec![("corpid", Value::Int(1))]);
        assert!(lifecycle::cancel_order(&Identity::of_client(&issuer), &test_db, &issuer.world, &issuer.sde, order.id, remote.region).await.is_err());
        place_buy(&Identity::of_client(&buyer), &test_db, &buyer.world, &buyer.sde, BuyRequest { station_id: remote.id, type_id: TYPE_PLEX.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        let remaining = orders::my_order(&test_db, f.issuer.character, order.id).await.unwrap().unwrap();
        assert_eq!((remaining.vol_remaining, remaining.plex_escrow), (1, 1));
        let account = accounts::ensure(&test_db, f.corporation, key).await.unwrap().id;
        let fills = listings::corporation_transactions(&test_db, f.corporation, &[key], 0).await.unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!((fills[0].seller_account_id, fills[0].station_id, fills[0].quantity), (Some(account), f.at.id, 1));
        assert_eq!(plex::balance(&test_db, f.counterparty.account).await.unwrap(), (buyer_vault.parse::<i128>().unwrap() + 1).to_string());
        assert_eq!(plex::balance(&test_db, f.issuer.account).await.unwrap(), vault);
        assert_eq!(characters::balance(&test_db, f.issuer.character).await.unwrap(), personal);
        fixture::assign_corporation(&test_db, f.issuer.character, f.corporation, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
        issuer.session.apply(vec![("corpid", Value::Int(f.corporation))]);
        lifecycle::cancel_order(&Identity::of_client(&issuer), &test_db, &issuer.world, &issuer.sde, order.id, remote.region).await.unwrap();
        assert_eq!(loading::load_items(&test_db, ItemQuery::OwnerLocationFlagTypeDescending(f.corporation, f.office, FLAG_CORP_SAG1, TYPE_PLEX)).await.unwrap().iter().map(|i| i128::from(i.quantity)).sum::<i128>(), 2);
        assert_eq!(plex::balance(&test_db, f.issuer.account).await.unwrap(), vault);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn corporate_plex_rejects_wrong_source_and_authority_without_mutation() {
    use infinity_core::consts::TYPE_PLEX;
    use crate::dao::items::transfers;
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut f = CorporateFixture::create(&db).await;
    f.type_id = TYPE_PLEX;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut issuer, _) = client(test_db.clone(), f.issuer.character).await;
        issuer.session.apply(vec![("corpid", Value::Int(f.corporation)), ("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let personal = transfers::insert(&test_db, TYPE_PLEX, f.issuer.character, f.at.id, FLAG_HANGAR, 2).await.unwrap();
        let corporate = transfers::insert(&test_db, TYPE_PLEX, f.corporation, f.office, FLAG_CORP_SAG1, 2).await.unwrap();
        let delivery = transfers::insert(&test_db, TYPE_PLEX, f.corporation, f.at.id, FLAG_CORP_DELIVERIES, 2).await.unwrap();
        let request = SellRequest { item_id: corporate, station_id: f.at.id, type_id: TYPE_PLEX.into(), price: 100000.0, quantity: 1, duration: 1, office_id: Some(f.office), expected_brokers_fee: None };
        let identity = Identity::of_client(&issuer);
        let key = ACCOUNT_CASH + 1;
        let before = f.fingerprint(&test_db).await;
        for (invalid, wallet) in [(SellRequest { item_id: personal, ..request }, Some(key)), (SellRequest { item_id: delivery, ..request }, Some(key)), (SellRequest { office_id: None, ..request }, Some(key)), (request, Some(ACCOUNT_CASH - 1)), (request, None)] {
            assert!(place_sell(&identity, &test_db, &issuer.world, &issuer.sde, invalid, wallet).await.is_err());
            assert_eq!(f.fingerprint(&test_db).await, before);
        }
        fixture::assign_corporation(&test_db, f.issuer.character, f.corporation, infinity_core::consts::ROLE_TRADER | infinity_core::consts::ROLE_ACCOUNT_CAN_TAKE_1).await.unwrap();
        let wrong_division = f.fingerprint(&test_db).await;
        assert!(place_sell(&identity, &test_db, &issuer.world, &issuer.sde, request, Some(key)).await.is_err());
        assert_eq!(f.fingerprint(&test_db).await, wrong_division);
        fixture::assign_corporation(&test_db, f.issuer.character, f.corporation, 0).await.unwrap();
        let revoked = f.fingerprint(&test_db).await;
        assert!(place_sell(&identity, &test_db, &issuer.world, &issuer.sde, request, Some(key)).await.is_err());
        assert_eq!(f.fingerprint(&test_db).await, revoked);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}
