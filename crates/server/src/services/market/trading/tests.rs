//! Real database regressions for market orders on types without a market group.
use infinity_core::consts::FLAG_HANGAR;
use crate::db::Db;
use crate::services::{Call, CallError, market, tests::{client, fixture}};
use marshal::Value;
use std::collections::HashSet;

#[test]
fn buy_quantity_admits_wide_values_and_rejects_zero() {
    crate::tests::init_config();
    for quantity in [i64::from(i32::MAX) + 1, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX] {
        let request = crate::services::market::BuyRequest { station_id: 1, type_id: 1, price: 1.0, quantity, range: crate::services::market::RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None };
        assert!(super::validate_buy(request).is_ok());
        assert!(super::validate_buy(crate::services::market::BuyRequest { quantity: 0, ..request }).is_err());
    }
}

async fn snapshot(db: &Db, character: i64) -> String {
    db.query_one("SELECT json_build_object(
        'balance', (SELECT balance::text FROM game.characters WHERE character_id=$1),
        'accounts', (SELECT COALESCE(json_agg(row_to_json(a) ORDER BY a.account_id), '[]'::json) FROM game.wallet_accounts a WHERE a.owner_id=$1),
        'journal', (SELECT COALESCE(json_agg(row_to_json(j) ORDER BY j.journal_id), '[]'::json) FROM game.wallet_journal j JOIN game.wallet_accounts a ON a.account_id=j.account_id WHERE a.owner_id=$1),
        'orders', (SELECT COALESCE(json_agg(row_to_json(o) ORDER BY o.order_id), '[]'::json) FROM game.market_orders o WHERE o.char_id=$1),
        'history', (SELECT COALESCE(json_agg(row_to_json(h) ORDER BY h.order_id), '[]'::json) FROM game.market_order_history h WHERE h.char_id=$1),
        'fills', (SELECT COALESCE(json_agg(row_to_json(t) ORDER BY t.transaction_id), '[]'::json) FROM game.market_transactions t WHERE t.buyer_id=$1 OR t.seller_id=$1),
        'items', (SELECT COALESCE(json_agg(row_to_json(i) ORDER BY i.item_id), '[]'::json) FROM game.items i WHERE i.owner_id=$1),
        'progress', (SELECT COALESCE(json_agg(row_to_json(e) ORDER BY e.event_id), '[]'::json) FROM game.progress_events e WHERE e.subject_character_id=$1 OR (e.kind='WalletChanged' AND (e.payload->'WalletChanged'->>'owner_id')::bigint=$1)),
        'deliveries', (SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.consumer_key, d.event_id), '[]'::json) FROM game.progress_deliveries d JOIN game.progress_events e ON e.event_id=d.event_id WHERE e.subject_character_id=$1 OR (e.kind='WalletChanged' AND (e.payload->'WalletChanged'->>'owner_id')::bigint=$1))
    )::text AS snapshot", &[&character]).await.unwrap().get("snapshot")
}

async fn exercise(nonmarket: bool) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let pilot = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), pilot.character).await;
        c.login.user_id = pilot.account;
        let station = c.sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some() && s.owner_id.is_some()).expect("SDE station");
        let (station_id, system_id) = (station.station_id, station.solar_system_id.unwrap());
        c.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system_id))]);
        let occupied: HashSet<i32> = test_db.query("SELECT DISTINCT type_id FROM game.market_orders", &[]).await.unwrap().iter().map(|r| r.get("type_id")).collect();
        let ty = if nonmarket {
            c.sde.types.iter().find(|t| t.type_id > 0 && t.market_group_id.is_none() && c.sde.is_tradable(t.type_id) && !occupied.contains(&t.type_id)).expect("unoccupied SDE type without a market group").type_id
        } else {
            (1..=i32::MAX).rev().find(|&id| c.sde.types.get(id).is_none() && !occupied.contains(&id)).expect("unknown i32 SDE type")
        };
        test_db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=$1", &[&pilot.character]).await.unwrap();
        let item = if nonmarket {
            Some(test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&ty, &pilot.character, &station_id, &FLAG_HANGAR]).await.unwrap().get::<_, i64>("item_id"))
        } else { None };
        let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
        c.world.register_pilot(pilot.character, sender);
        let before = snapshot(&test_db, pilot.character).await;
        let kwargs = Value::None;
        let buy = [Value::Int(station_id), Value::Int(i64::from(ty)), Value::Float(100000.0), Value::Int(1), Value::Int(-1), Value::Int(1), Value::Int(1), Value::Bool(false), Value::None];
        let call = Call { service: "marketProxy", method: "PlaceBuyOrder", args: &buy, kwargs: &kwargs, bound: false, bind_params: None };
        let buy_result = market::dispatch(&mut c, &call).await.expect("market dispatch");
        let after_buy = snapshot(&test_db, pilot.character).await;
        let line = crate::services::keyval([("stationID", Value::Int(station_id)), ("typeID", Value::Int(i64::from(ty))), ("price", Value::Float(100000.0)), ("quantity", Value::Int(1))]);
        let batch = [Value::Int(station_id), Value::List(vec![line]), Value::Bool(false)];
        let call = Call { service: "marketProxy", method: "BuyMultipleItems", args: &batch, kwargs: &kwargs, bound: false, bind_params: None };
        let batch_result = market::dispatch(&mut c, &call).await.expect("market dispatch");
        let after_batch = snapshot(&test_db, pilot.character).await;
        let sell_result = if let Some(item) = item {
            let line = crate::services::keyval([("stationID", Value::Int(station_id)), ("typeID", Value::Int(i64::from(ty))), ("itemID", Value::Int(item)), ("price", Value::Float(100000.0)), ("quantity", Value::Int(1)), ("officeID", Value::None)]);
            let sell = [Value::List(vec![line]), Value::Bool(false), Value::Int(1), Value::None];
            let call = Call { service: "marketProxy", method: "PlaceMultiSellOrder", args: &sell, kwargs: &kwargs, bound: false, bind_params: None };
            Some(market::dispatch(&mut c, &call).await.expect("market dispatch"))
        } else { None };
        let after_sell = snapshot(&test_db, pilot.character).await;
        let notice = notices.try_recv();
        c.world.unregister_pilot_generation(pilot.character, None);
        macro_rules! assert_refusal {
            ($result:expr) => {
                match $result {
                    Err(CallError::User { msg, dict }) => {
                        assert_eq!(msg, infinity_core::messages::MSG_MKT_ORDER_DID_NOT_MATCH);
                        assert!(dict.is_empty());
                    }
                    other => panic!("expected market order refusal, got {other:?}"),
                }
            };
        }
        assert_refusal!(buy_result);
        assert_refusal!(batch_result);
        if let Some(result) = sell_result { assert_refusal!(result); }
        assert_eq!(before, after_buy, "ordinary buy changed durable state");
        assert_eq!(after_buy, after_batch, "bulk buy changed durable state");
        assert_eq!(after_batch, after_sell, "sell changed durable state");
        assert!(notice.is_err(), "refusal must not notify the pilot");
        assert!(c.outbox.is_empty(), "refusal must not queue client packets");
    }).await;
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1) OR market_transaction_id IN (SELECT transaction_id FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1)", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE char_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.market_order_history WHERE char_id=$1", &[&pilot.character]).await.unwrap();
    fixture::cleanup(&db, pilot).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn unknown_type_orders_are_refused_without_effects() { exercise(false).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn nonmarket_type_orders_are_refused_without_effects() { exercise(true).await; }
