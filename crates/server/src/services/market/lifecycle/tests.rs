//! Real database regression for repricing an overdue personal market order.
use crate::client::filetime_now;
use infinity_core::consts::FILETIME_DAY;
use crate::db::Db;
use crate::services::{Call, CallError, market, tests::{client, fixture}};
use marshal::Value;

#[test]
fn owned_order_deadline_is_strict_and_checked() {
    let issued = 10;
    let deadline = issued + FILETIME_DAY;
    let mut order = crate::dao::market::orders::Order {
        id: 1, type_id: 1, char_id: Some(1),
        at: crate::dao::market::orders::Station { id: 1, system: 1, constellation: 1, region: 1, owner: 1 },
        corporation_id: None, wallet_key: infinity_core::consts::ACCOUNT_CASH,
        source_location_id: None, source_flag_id: None,
        price: crate::isk::IskAmount::from_whole_isk(1).unwrap(),
        vol_entered: 1, vol_remaining: 1, min_volume: 1, bid: true, range: -1,
        duration: 1, issued: Some(issued), escrow: crate::isk::IskAmount::from_whole_isk(1).unwrap(),
        scope: crate::dao::market::orders::MarketScope::Regional, vault_user_id: None,
        plex_escrow: 0, created_at: Some(issued), expires_at: None,
    };
    assert!(super::owned_order_active(&order, deadline - 1));
    assert!(!super::owned_order_active(&order, deadline));
    assert!(!super::owned_order_active(&order, deadline + 1));
    order.issued = None;
    assert!(!super::owned_order_active(&order, deadline - 1));
    order.issued = Some(i64::MAX);
    assert!(super::owned_order_active(&order, deadline));
    order.issued = Some(0);
    order.duration = i64::MAX;
    assert!(super::owned_order_active(&order, deadline));
    order.expires_at = Some(deadline);
    assert!(!super::owned_order_active(&order, deadline));
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

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn overdue_personal_bid_reprice_is_refused_without_effects() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let pilot = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), pilot.character).await;
        c.login.user_id = pilot.account;
        let at = fixture::isolated_market_stations(&test_db, &c.sde).await.unwrap().next().expect("isolated NPC station");
        let (station_id, system_id) = (at.id, at.system);
        c.session.apply(vec![("stationid", Value::Int(station_id)), ("solarsystemid2", Value::Int(system_id))]);
        let type_id = fixture::market_type_ids(&c.sde).next().expect("tradable market SDE type");
        test_db.execute("UPDATE game.characters SET balance=10000000 WHERE character_id=$1", &[&pilot.character]).await.unwrap();
        let kwargs = Value::Dict(vec![]);
        let args = [Value::Int(station_id), Value::Int(i64::from(type_id)), Value::Float(100000.0), Value::Int(1), Value::Int(-1), Value::Int(1), Value::Int(1), Value::Bool(false), Value::None];
        let place = Call { service: "marketProxy", method: "PlaceBuyOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        market::dispatch(&mut c, &place).await.expect("market dispatch").expect("place personal bid");
        let order = test_db.query_one("SELECT order_id FROM game.market_orders WHERE char_id=$1 AND bid", &[&pilot.character]).await.unwrap().get::<_, i64>("order_id");
        let captured_now = filetime_now();
        test_db.execute("UPDATE game.market_orders SET issued=$2, expires_at=$2 + duration::bigint * $3 WHERE order_id=$1", &[&order, &(captured_now - FILETIME_DAY), &FILETIME_DAY]).await.unwrap();
        let row = test_db.query_one("SELECT order_id, bid, station_id, solar_system_id, price::text AS price, range, vol_remaining, issued, expires_at AS deadline FROM game.market_orders WHERE order_id=$1", &[&order]).await.unwrap();
        assert!(row.get::<_, i64>("deadline") <= captured_now);
        let price = crate::isk::IskAmount::parse_decimal(row.get("price")).unwrap().wire_isk();
        let args = [Value::Int(row.get("order_id")), Value::Float(110000.0), Value::Bool(row.get("bid")), Value::Int(row.get("station_id")), Value::Int(row.get("solar_system_id")), Value::Float(price), Value::Int(i64::from(row.get::<_, i32>("range"))), Value::Int(row.get::<_, i64>("vol_remaining")), Value::Int(row.get("issued"))];
        let before = snapshot(&test_db, pilot.character).await;
        let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
        c.world.register_pilot(pilot.character, sender);
        let modify = Call { service: "marketProxy", method: "ModifyCharOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let response = market::dispatch(&mut c, &modify).await.expect("market dispatch");
        let after = snapshot(&test_db, pilot.character).await;
        let notice = notices.try_recv();
        c.world.unregister_pilot_generation(pilot.character, None);
        match response {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".to_string(), Value::unicode(infinity_core::messages::MARKET_ORDER_CLOSED))]);
            }
            other => panic!("expected closed-order refusal, got {other:?}"),
        }
        assert_eq!(before, after, "refusal must not change durable state");
        assert!(notice.is_err(), "refusal must not notify the pilot");
    }).await;
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1) OR market_transaction_id IN (SELECT transaction_id FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1)", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.market_transactions WHERE buyer_id=$1 OR seller_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.market_orders WHERE char_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.market_order_history WHERE char_id=$1", &[&pilot.character]).await.unwrap();
    fixture::cleanup(&db, pilot).await.unwrap();
    result.unwrap();
}
