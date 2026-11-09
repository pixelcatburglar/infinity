//! Real market batch RPC results and independently committed sell children.
use super::*;
use super::corporate_fixture::CorporateFixture;
use infinity_core::consts::FLAG_HANGAR;
use crate::services::tests::{client, fixture};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn refused_late_bulk_buy_rolls_back_all_economy_and_notifications() {
    use super::super::trading::place_sell;
    use crate::services::identity::Identity;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _b) = client(test_db.clone(), f.counterparty.character).await;
        for c in [&mut buyer, &mut seller] {
            c.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        }
        let second = fixture::market_type_ids(&buyer.sde).find(|&ty| ty != f.type_id).expect("second tradable market SDE type");
        for ty in [f.type_id, second] {
            let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&ty, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
            place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        }
        test_db.execute("UPDATE game.characters SET balance=50000 WHERE character_id=$1", &[&f.issuer.character]).await.unwrap();
        let original = match buy_rpc(&mut buyer, f.at.id, vec![buy_line(f.at.id, second, 100000.0, 1)], Value::Bool(false)).await {
            Err(CallError::User { msg, dict }) => (msg, dict),
            _ => panic!("first-fill insufficient funds must return a UserError"),
        };
        test_db.execute("UPDATE game.characters SET balance=150000 WHERE character_id=$1", &[&f.issuer.character]).await.unwrap();
        let (buyer_tx, mut buyer_rx) = tokio::sync::mpsc::unbounded_channel();
        let (seller_tx, mut seller_rx) = tokio::sync::mpsc::unbounded_channel();
        buyer.world.register_pilot(f.issuer.character, buyer_tx);
        buyer.world.register_pilot(f.counterparty.character, seller_tx);
        let before = f.fingerprint(&test_db).await;
        let error = buy_rpc(&mut buyer, f.at.id, vec![buy_line(f.at.id, f.type_id, 100000.0, 1), buy_line(f.at.id, second, 100000.0, 1)], Value::Bool(false)).await;
        match error {
            Err(CallError::User { msg, dict }) => assert_eq!((msg, dict), original),
            _ => panic!("late insufficient funds must return the original UserError"),
        }
        assert_eq!(f.fingerprint(&test_db).await, before);
        assert!(buyer_rx.try_recv().is_err());
        assert!(seller_rx.try_recv().is_err());
        assert!(buyer.outbox.is_empty());
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn bulk_buy_wire_validation_precedes_economic_writes() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _peer) = client(test_db.clone(), f.issuer.character).await;
        buyer.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let good = buy_line(f.at.id, f.type_id, 100000.0, 1);
        let overflow = crate::services::keyval([("stationID", Value::Int(f.at.id)), ("typeID", Value::Int(i64::MAX)), ("price", Value::Float(100000.0)), ("quantity", Value::Int(1))]);
        let invalid_quantity = buy_line(f.at.id, f.type_id, 100000.0, 0);
        let nan = crate::services::keyval([("stationID", Value::Int(f.at.id)), ("typeID", Value::Int(i64::from(f.type_id))), ("price", Value::Float(f64::NAN)), ("quantity", Value::Int(1))]);
        let cases = vec![
            (vec![good.clone(); 101], Value::Bool(false)),
            (vec![good.clone(), good.clone()], Value::Bool(false)),
            (vec![good.clone()], Value::Int(0)),
            (vec![overflow], Value::Bool(false)),
            (vec![invalid_quantity], Value::Bool(false)),
            (vec![nan], Value::Bool(false)),
        ];
        let before = f.fingerprint(&test_db).await;
        for (rows, corporate) in cases {
            assert!(matches!(buy_rpc(&mut buyer, f.at.id, rows, corporate).await, Err(CallError::User { .. })));
            assert_eq!(f.fingerprint(&test_db).await, before);
        }
        for (range, min_volume) in [(i64::MAX, 1), (RANGE_STATION, 0), (RANGE_STATION, 2)] {
            let args = [Value::Int(f.at.id), Value::Int(i64::from(f.type_id)), Value::Float(100000.0), Value::Int(1), Value::Int(range), Value::Int(min_volume), Value::Int(0), Value::Bool(false), Value::None];
            let kwargs = Value::None;
            let call = Call { service: "marketProxy", method: "PlaceBuyOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert!(matches!(super::super::market_proxy::dispatch(&mut buyer, &call).await.unwrap(), Err(CallError::User { .. })));
            assert_eq!(f.fingerprint(&test_db).await, before);
        }
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn accepted_bulk_buys_return_unique_ids_and_actual_expiry_remainders() {
    use super::super::trading::place_sell;
    use crate::services::identity::Identity;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _b) = client(test_db.clone(), f.counterparty.character).await;
        for c in [&mut buyer, &mut seller] {
            c.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        }
        let mut types = vec![f.type_id];
        types.extend(fixture::market_type_ids(&buyer.sde).filter(|&ty| ty != f.type_id).take(2));
        let other_station = fixture::isolated_market_stations(&test_db, &buyer.sde).await.unwrap()
            .find(|station| station.id != f.at.id && station.system == f.at.system && station.region == f.at.region)
            .expect("second isolated station in the same system");
        assert_eq!(types.len(), 3);
        for &ty in &types[..2] {
            let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&ty, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
            place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: ty.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        buyer.world.register_pilot(f.issuer.character, tx);
        let response = buy_rpc(&mut buyer, f.at.id, vec![buy_line(f.at.id, types[0], 100000.0, 1), buy_line(f.at.id, types[1], 100000.0, 2), buy_line(other_station.id, types[2], 100000.0, 2)], Value::Bool(false)).await.unwrap();
        let response = marshal::loads(&marshal::dumps(&response)).unwrap();
        let rows = response.items().unwrap();
        assert_eq!(rows.len(), 3);
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(name, _)| name == "OnOwnOrdersChanged").collect();
        let mut ids = HashSet::new();
        for (index, row) in rows.iter().enumerate() {
            assert!(matches!(row, Value::Instance(class, _) if **class == Value::str("utillib.KeyVal")));
            let id = crate::services::value_field(row, "orderID").unwrap().as_int().unwrap();
            assert!(id > 0 && ids.insert(id));
            let remaining = [0, 1, 2][index];
            assert_eq!(crate::services::value_field(row, "typeID"), Some(&Value::Int(i64::from(types[index]))));
            assert_eq!(crate::services::value_field(row, "stationID"), Some(&Value::Int(if index == 2 { other_station.id } else { f.at.id })));
            assert_eq!(crate::services::value_field(row, "price"), Some(&Value::Float(100000.0)));
            assert_eq!(crate::services::value_field(row, "escrow"), Some(&Value::Float(0.0)));
            assert_eq!(crate::services::value_field(row, "bid"), Some(&Value::Bool(true)));
            assert_eq!(crate::services::value_field(row, "volRemaining"), Some(&Value::Int(remaining)));
            let history = test_db.query_one("SELECT type_id,vol_remaining FROM game.market_order_history WHERE order_id=$1", &[&id]).await.unwrap();
            assert_eq!(history.get::<_, i32>("type_id"), types[index]);
            assert_eq!(history.get::<_, i64>("vol_remaining"), remaining);
            assert!(test_db.query("SELECT order_id FROM game.market_orders WHERE order_id=$1", &[&id]).await.unwrap().is_empty());
            assert!(notices.iter().any(|(_, args)| {
                let wire = marshal::loads(&marshal::dumps(&Value::List(args.clone()))).unwrap();
                let parts = wire.items().unwrap();
                parts.get(1).and_then(Value::as_text).as_deref() == Some("Expiry") &&
                    parts.first().and_then(Value::items).is_some_and(|list| list.iter().any(|notice| {
                        crate::services::value_field(notice, "orderID") == Some(&Value::Int(id)) &&
                        crate::services::value_field(notice, "typeID") == Some(&Value::Int(i64::from(types[index]))) &&
                        crate::services::value_field(notice, "volRemaining") == Some(&Value::Int(remaining))
                    }))
            }));
        }
        let fills = test_db.query("SELECT type_id,quantity FROM game.market_transactions WHERE buyer_id=$1", &[&f.issuer.character]).await.unwrap();
        assert_eq!(fills.len(), 2);
        assert!(fills.iter().all(|row| row.get::<_, i64>("quantity") == 1));
        assert_eq!(test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get::<_, String>("balance"), "9800000.00");
        for &ty in &types[..2] {
            assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=$3 AND flag_id=$4", &[&f.issuer.character, &ty, &f.at.id, &FLAG_HANGAR]).await.unwrap().get::<_, i64>("quantity"), 1);
        }
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn concurrent_bulk_and_single_buy_share_serial_lock() {
    use super::super::trading::{place_buy, place_sell};
    use crate::services::identity::Identity;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut batch, _a) = client(test_db.clone(), f.issuer.character).await;
        let (mut single, _b) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _c) = client(test_db.clone(), f.counterparty.character).await;
        for c in [&mut batch, &mut single, &mut seller] {
            c.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        }
        let item: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        place_sell(&Identity::of_client(&seller), &seller.db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
        let request = BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None };
        let identity = Identity::of_client(&single);
        let (batch_result, single_result) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(buy_rpc(&mut batch, f.at.id, vec![buy_line(f.at.id, f.type_id, 100000.0, 1)], Value::Bool(false)),
                place_buy(&identity, &single.db, &single.world, &single.sde, request, None))
        }).await.expect("market operations deadlocked");
        assert!(batch_result.is_ok());
        assert!(single_result.is_ok() || matches!(single_result, Err(CallError::User { .. })));
        let rows = test_db.query("SELECT quantity FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$2", &[&f.issuer.character, &f.counterparty.character]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, i64>("quantity"), 1);
        assert_eq!(test_db.query_one("SELECT sum(quantity)::text AS quantity FROM game.items WHERE owner_id=$1 AND type_id=$2 AND location_id=$3 AND flag_id=$4", &[&f.issuer.character, &f.type_id, &f.at.id, &FLAG_HANGAR]).await.unwrap().get::<_, Option<String>>("quantity").as_deref(), Some("1"));
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn multi_sell_keeps_committed_success_and_reports_real_failure() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut seller, _peer) = client(test_db.clone(), f.issuer.character).await;
        seller.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        let first: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.issuer.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let second: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let good = sell_line(f.at.id, f.type_id, first, 100000.0, 1, None);
        let bad = sell_line(f.at.id, f.type_id, second, 100000.0, 1, None);
        let before_duplicate = f.fingerprint(&test_db).await;
        assert!(matches!(sell_rpc(&mut seller, vec![good.clone(), good.clone()], false, 1).await, Err(CallError::User { .. })));
        assert_eq!(f.fingerprint(&test_db).await, before_duplicate);
        let (original_msg, original_dict) = match sell_rpc(&mut seller, vec![bad.clone()], false, 1).await {
            Err(CallError::User { msg, dict }) => (msg, dict),
            _ => panic!("single invalid source must return its original UserError"),
        };
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get("balance");
        match sell_rpc(&mut seller, vec![good.clone(), bad.clone()], false, 1).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::market::sell_failures(&[(second, i64::from(f.type_id), 1)])))]);
            }
            _ => panic!("mixed sell must report the actual failed child"),
        }
        let rows = test_db.query("SELECT order_id,vol_remaining FROM game.market_orders WHERE char_id=$1 AND type_id=$2 AND NOT bid", &[&f.issuer.character, &f.type_id]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, i64>("vol_remaining"), 1);
        assert!(test_db.query("SELECT item_id FROM game.items WHERE item_id=$1", &[&first]).await.unwrap().is_empty());
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&second]).await.unwrap().get::<_, i64>("quantity"), 1);
        let after: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get("balance");
        assert_ne!(before, after);
        assert!(!test_db.query("SELECT j.order_id FROM game.wallet_journal j JOIN game.wallet_accounts a USING(account_id) WHERE a.owner_id=$1 AND j.order_id=$2", &[&f.issuer.character, &rows[0].get::<_, i64>("order_id")]).await.unwrap().is_empty());
        match sell_rpc(&mut seller, vec![bad.clone()], false, 1).await {
            Err(CallError::User { msg, dict }) => { assert_eq!(msg, original_msg); assert_eq!(dict, original_dict); }
            _ => panic!("single invalid source must retain its original UserError"),
        }
        assert!(matches!(sell_rpc(&mut seller, vec![good, bad], false, 1).await, Err(CallError::User { .. })));
        assert_eq!(test_db.query("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND type_id=$2 AND NOT bid", &[&f.issuer.character, &f.type_id]).await.unwrap().len(), 1);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn immediate_multi_sell_retains_matched_fill_after_other_child_fails() {
    use crate::services::identity::Identity;
    use super::super::trading::place_buy;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut seller, _seller_peer) = client(test_db.clone(), f.issuer.character).await;
        let (mut buyer, _buyer_peer) = client(test_db.clone(), f.counterparty.character).await;
        for c in [&mut seller, &mut buyer] {
            c.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        }
        let bid = place_buy(&Identity::of_client(&buyer), &test_db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 100000.0, quantity: 1, range: RANGE_STATION, min_volume: 1, duration: 1, expected_brokers_fee: None }, None).await.unwrap();
        let good_item: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.issuer.character, &f.at.id, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let bad_item: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.office, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let good = sell_line(f.at.id, f.type_id, good_item, 100000.0, 1, None);
        let bad = sell_line(f.at.id, f.type_id, bad_item, 100000.0, 1, None);
        let original = match sell_rpc(&mut seller, vec![bad.clone()], false, 0).await {
            Err(CallError::User { msg, dict }) => (msg, dict),
            _ => panic!("single invalid source must return its original UserError"),
        };
        let before: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get("balance");
        match sell_rpc(&mut seller, vec![good.clone(), bad.clone()], false, 0).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(infinity_core::messages::market::sell_failures(&[(bad_item, i64::from(f.type_id), 1)])))]);
            }
            _ => panic!("mixed sell must report the failed child"),
        }
        assert_eq!(test_db.query("SELECT transaction_id FROM game.market_transactions WHERE order_id=$1 AND buyer_id=$2 AND seller_id=$3", &[&bid.id, &f.counterparty.character, &f.issuer.character]).await.unwrap().len(), 1);
        assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&good_item]).await.unwrap().is_none());
        assert_eq!(test_db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&bad_item]).await.unwrap().get::<_, i64>("quantity"), 1);
        assert_eq!(test_db.query_one("SELECT COALESCE(sum(quantity),0)::text AS quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&f.counterparty.character, &f.at.id, &f.type_id, &FLAG_HANGAR]).await.unwrap().get::<_, String>("quantity"), "1");
        let after: String = test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get("balance");
        assert_ne!(before, after);
        match sell_rpc(&mut seller, vec![bad.clone()], false, 0).await {
            Err(CallError::User { msg, dict }) => assert_eq!((msg, dict), original),
            _ => panic!("single invalid source must retain its original UserError"),
        }
        assert!(matches!(sell_rpc(&mut seller, vec![good, bad], false, 0).await, Err(CallError::User { .. })));
        assert_eq!(test_db.query("SELECT transaction_id FROM game.market_transactions WHERE order_id=$1", &[&bid.id]).await.unwrap().len(), 1);
        assert_eq!(test_db.query_one("SELECT balance::text AS balance FROM game.characters WHERE character_id=$1", &[&f.issuer.character]).await.unwrap().get::<_, String>("balance"), after);
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}
