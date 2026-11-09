//! Real personal-market accounting across both matching directions.
use super::*;
use super::super::trading::{place_buy, place_sell};
use infinity_core::consts::FLAG_HANGAR;
use crate::services::{identity::Identity, tests::{client, fixture}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn personal_cash_and_escrow_fills_have_real_accounts_and_journals() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let seller = fixture::create(&db).await.unwrap();
    let buyer = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut sc, _sp) = client(test_db.clone(), seller.character).await;
        let (mut bc, _bp) = client(test_db.clone(), buyer.character).await;
        let at = fixture::isolated_market_stations(&test_db, &sc.sde).await.unwrap().next().expect("isolated NPC station");
        let (station, station_owner, system, region) = (at.id, at.owner, at.system, at.region);
        for c in [&mut sc, &mut bc] { c.session.apply(vec![("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]); }
        let ty = fixture::market_type_ids(&sc.sde).next().expect("tradable market SDE type");
        for id in [seller.character, buyer.character] {
            test_db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=$1", &[&id]).await.unwrap();
        }
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&ty,&seller.character,&station,&FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, SellRequest { item_id: item, station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        place_buy(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, BuyRequest { station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        place_buy(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, BuyRequest { station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        let buy_order: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND bid", &[&buyer.character]).await.unwrap().get("order_id");
        let before: i64 = test_db.query_one("SELECT count(*) AS n FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1", &[&buyer.character]).await.unwrap().get("n");
        place_sell(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, SellRequest { item_id: item, station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 0, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let after: i64 = test_db.query_one("SELECT count(*) AS n FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1", &[&buyer.character]).await.unwrap().get("n");
        let counterparties = test_db.query_one("SELECT count(*) FILTER (WHERE j.entry_type_id=$2) AS fees, bool_and(j.owner_id2=$3) FILTER (WHERE j.entry_type_id=$2) AS fee_owner, count(*) FILTER (WHERE j.entry_type_id=$4) AS taxes, bool_and(j.owner_id2=$5) FILTER (WHERE j.entry_type_id=$4) AS tax_owner FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=ANY($1)", &[&vec![buyer.character,seller.character],&REF_BROKER_FEE,&station_owner,&infinity_core::consts::REF_TRANSACTION_TAX,&OWNER_SCC]).await.unwrap();
        assert!(counterparties.get::<_,i64>("fees") >= 2);
        assert!(counterparties.get::<_,bool>("fee_owner"));
        assert!(counterparties.get::<_,i64>("taxes") > 0);
        assert!(counterparties.get::<_,bool>("tax_owner"));
        let entries = test_db.query("SELECT a.owner_id, j.actor_id, j.owner_id1, j.owner_id2, j.reference_id, j.market_transaction_id, j.order_id, j.amount::text AS amount FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=ANY($1) AND j.entry_type_id=$2 ORDER BY j.journal_id", &[&vec![buyer.character,seller.character],&REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(entries.len(), 3);
        for row in entries {
            let owner: i64 = row.get("owner_id");
            assert_eq!(row.get::<_, i64>("actor_id"), owner);
            assert_eq!(row.get::<_, i64>("owner_id1"), buyer.character);
            assert_eq!(row.get::<_, i64>("owner_id2"), seller.character);
            assert_eq!(row.get::<_, i64>("reference_id"), row.get::<_, Option<i64>>("market_transaction_id").unwrap());
            assert!(row.get::<_, Option<i64>>("order_id").is_some());
            assert_eq!(row.get::<_, String>("amount"), if owner == buyer.character { "-100000.00" } else { "100000.00" });
        }
        let escrow = test_db.query_one("SELECT j.actor_id, j.owner_id1, j.owner_id2, j.reference_id, j.order_id, j.amount::text AS amount FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$2 AND j.order_id=$3", &[&buyer.character,&REF_MARKET_ESCROW,&buy_order]).await.unwrap();
        assert_eq!(escrow.get::<_, i64>("actor_id"), buyer.character);
        assert_eq!(escrow.get::<_, i64>("owner_id1"), buyer.character);
        assert_eq!(escrow.get::<_, i64>("owner_id2"), OWNER_SCC);
        assert_eq!(escrow.get::<_, i64>("reference_id"), -1);
        assert_eq!(escrow.get::<_, Option<i64>>("order_id"), Some(buy_order));
        assert_eq!(escrow.get::<_, String>("amount"), "-100000.00");
        let tax = test_db.query_one("SELECT count(*) AS n, bool_and(j.owner_id1=$1 AND j.owner_id2=$2 AND j.actor_id=$1) AS correct FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$3", &[&seller.character,&OWNER_SCC,&infinity_core::consts::REF_TRANSACTION_TAX]).await.unwrap();
        assert_eq!(tax.get::<_, i64>("n"), 2);
        assert!(tax.get::<_, bool>("correct"));
        assert_eq!(before, after, "escrow consumption is not a cash mutation");
        let rows = test_db.query("SELECT buyer_account_id,seller_account_id FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$2 ORDER BY transaction_id", &[&buyer.character,&seller.character]).await.unwrap();
        assert_eq!(rows.len(), 2);
        for (viewer, character) in [(&bc, buyer.character), (&sc, seller.character)] {
            let wire = super::super::lists::transactions(viewer, Some(0)).await.unwrap();
            let Value::Reduce { data, .. } = wire else { panic!("expected transaction CRowset") };
            let args = data.items().unwrap()[1].items().unwrap();
            let descriptor = DbRowDescriptor::from_value(&args[0]).unwrap();
            let actual = args[1].items().unwrap();
            assert_eq!(actual.len(), 2);
            for row in actual {
                assert_eq!(descriptor.get(row, "buyerID"), Some(Value::Int(buyer.character)));
                assert_eq!(descriptor.get(row, "sellerID"), Some(Value::Int(seller.character)));
                let own = descriptor.get(row, "accountID").unwrap();
                let party = if character == buyer.character { "buyerAccountID" } else { "sellerAccountID" };
                assert_eq!(Some(own.clone()), descriptor.get(row, party));
                assert_ne!(own, Value::Int(ACCOUNT_CASH.into()));
            }
        }
        for row in rows { let b: i64 = row.get("buyer_account_id"); let s: i64 = row.get("seller_account_id"); assert_ne!(b,s); }
        let sums = test_db.query("SELECT a.owner_id, sum(j.amount)::text AS total, (c.balance-10000000)::text AS delta FROM game.wallet_accounts a JOIN game.wallet_journal j USING(account_id) JOIN game.characters c ON c.character_id=a.owner_id WHERE a.owner_id=ANY($1) GROUP BY a.owner_id,c.balance", &[&vec![buyer.character,seller.character]]).await.unwrap();
        assert_eq!(sums.len(), 2);
        for row in sums { assert_eq!(row.get::<_, String>("total"),row.get::<_, String>("delta")); }
        place_buy(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, BuyRequest { station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        let repriced: i64 = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND bid", &[&buyer.character]).await.unwrap().get("order_id");
        crate::dao::market::tests::age_order_for_modification(&test_db, repriced).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, modify_request(&bc, repriced, 120000.0).await).await.unwrap();
        crate::dao::market::tests::age_order_for_modification(&test_db, repriced).await.unwrap();
        super::super::lifecycle::modify_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, modify_request(&bc, repriced, 110000.0).await).await.unwrap();
        super::super::lifecycle::cancel_order(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, repriced, region).await.unwrap();
        let escrow_rows = test_db.query("SELECT actor_id, owner_id1, owner_id2, reference_id, order_id, amount::text AS amount FROM game.wallet_journal WHERE order_id=$1 AND entry_type_id=$2 ORDER BY journal_id", &[&repriced,&REF_MARKET_ESCROW]).await.unwrap();
        assert_eq!(escrow_rows.len(), 4);
        for (row, amount) in escrow_rows.iter().zip(["-100000.00", "-20000.00", "10000.00", "110000.00"]) {
            assert_eq!(row.get::<_, i64>("actor_id"), buyer.character);
            assert_eq!(row.get::<_, i64>("owner_id1"), buyer.character);
            assert_eq!(row.get::<_, i64>("owner_id2"), OWNER_SCC);
            assert_eq!(row.get::<_, i64>("reference_id"), -1);
            assert_eq!(row.get::<_, Option<i64>>("order_id"), Some(repriced));
            assert_eq!(row.get::<_, String>("amount"), amount);
        }
        let fee_rows = test_db.query("SELECT owner_id2, reference_id, order_id FROM game.wallet_journal WHERE order_id=$1 AND entry_type_id=$2 ORDER BY journal_id", &[&repriced,&REF_BROKER_FEE]).await.unwrap();
        assert_eq!(fee_rows.len(), 3);
        for row in fee_rows {
            assert_eq!(row.get::<_, i64>("owner_id2"), station_owner);
            assert_eq!(row.get::<_, i64>("reference_id"), -1);
            assert_eq!(row.get::<_, Option<i64>>("order_id"), Some(repriced));
        }
        let stack: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&ty,&seller.character,&station,&FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, SellRequest { item_id: stack, station_id: station, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        place_sell(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, SellRequest { item_id: stack, station_id: station, type_id: ty.into(), price: 200000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        assert!(matches!(place_buy(&Identity::of_client(&sc), &sc.db, &sc.world, &sc.sde, BuyRequest { station_id: station, type_id: ty.into(), price: 200000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await, Err(CallError::User { .. })));
        test_db.execute("UPDATE game.characters SET balance=150000 WHERE character_id=$1", &[&buyer.character]).await.unwrap();
        place_buy(&Identity::of_client(&bc), &bc.db, &bc.world, &bc.sde, BuyRequest { station_id: station, type_id: ty.into(), price: 200000.0, quantity: 2, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        let row = test_db.query_one("SELECT (SELECT count(*) FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$2) AS fills, (SELECT count(*) FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$3) AS purchases", &[&buyer.character,&seller.character,&REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(row.get::<_, i64>("fills"), 3);
        assert_eq!(row.get::<_, i64>("purchases"), 2);
    }).await;
    cleanup_market_fixture(&db, seller).await.unwrap();
    cleanup_market_fixture(&db, buyer).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn npc_market_journal_owners_follow_buyer_and_seller() {
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
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&ty,&player.character,&at.id,&FLAG_HANGAR]).await.unwrap().get("item_id");
        place_buy(&Identity::of_client(&c), &c.db, &c.world, &c.sde, BuyRequest { station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, None).await.unwrap();
        place_sell(&Identity::of_client(&c), &c.db, &c.world, &c.sde, SellRequest { item_id: item, station_id: at.id, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 0, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let rows = test_db.query("SELECT j.actor_id, j.owner_id1, j.owner_id2, j.reference_id, j.market_transaction_id, j.order_id, j.amount::text AS amount FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.entry_type_id=$2 ORDER BY j.journal_id", &[&player.character,&REF_MARKET_TRANSACTION]).await.unwrap();
        assert_eq!(rows.len(), 2);
        for (row, buyer, seller, order_id, amount) in [(&rows[0], player.character, at.owner, ask, "-100000.00"), (&rows[1], at.owner, player.character, bid, "100000.00")] {
            assert_eq!(row.get::<_, i64>("actor_id"), player.character);
            assert_eq!(row.get::<_, i64>("owner_id1"), buyer);
            assert_eq!(row.get::<_, i64>("owner_id2"), seller);
            assert_eq!(row.get::<_, i64>("reference_id"), row.get::<_, Option<i64>>("market_transaction_id").unwrap());
            assert_eq!(row.get::<_, Option<i64>>("order_id"), Some(order_id));
            assert_eq!(row.get::<_, String>("amount"), amount);
        }
        let tax = test_db.query_one("SELECT count(*) AS n, bool_and(j.owner_id1=$1 AND j.owner_id2=$2) AS correct, (sum(j.amount)-(c.balance-10000000))::text AS difference FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) JOIN game.characters c ON c.character_id=a.owner_id WHERE a.owner_id=$1 AND j.entry_type_id=$3 GROUP BY c.balance", &[&player.character,&OWNER_SCC,&infinity_core::consts::REF_TRANSACTION_TAX]).await.unwrap();
        assert_eq!(tax.get::<_, i64>("n"), 1);
        assert!(tax.get::<_, bool>("correct"));
        assert_eq!(tax.get::<_, String>("difference"), "0.00");
    }).await;
    cleanup_market_fixture(&db, player).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE order_id=ANY($1)", &[&vec![ask,bid]]).await.unwrap();
    result.unwrap();
}
