use super::*;
use super::corporate_fixture::CorporateFixture;
use super::super::trading::{place_buy, place_sell};
use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_HANGAR};
use crate::services::identity::Identity;
use crate::services::tests::client;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn wide_order_fills_split_delivery_for_personal_and_corporate_buyers() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = CorporateFixture::create(&db).await;
    let test_db = db.clone();
    let scenario = tokio::spawn(async move {
        let (mut buyer, _) = client(test_db.clone(), f.issuer.character).await;
        let (mut seller, _) = client(test_db.clone(), f.counterparty.character).await;
        for c in [&mut buyer, &mut seller] {
            c.session.apply(vec![("stationid", Value::Int(f.at.id)), ("solarsystemid2", Value::Int(f.at.system))]);
        }
        test_db.execute("UPDATE game.characters SET balance=100000000000 WHERE character_id=ANY($1)", &[&vec![f.issuer.character, f.counterparty.character]]).await.unwrap();
        test_db.execute("UPDATE game.corp_wallets SET balance=100000000000 WHERE corporation_id=$1", &[&f.corporation]).await.unwrap();
        let quantity = i64::from(i32::MAX) + 1;
        for corporate in [false, true] {
            let owner = if corporate { f.corporation } else { f.issuer.character };
            let flag = if corporate { FLAG_CORP_DELIVERIES } else { FLAG_HANGAR };
            if corporate { buyer.session.apply(vec![("corpid", Value::Int(f.corporation))]); }
            let item: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id", &[&f.type_id, &f.counterparty.character, &f.at.id, &FLAG_HANGAR, &quantity]).await.unwrap().get("item_id");
            let merged: i64 = test_db.query_one("INSERT INTO game.items(type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,$5,false) RETURNING item_id", &[&f.type_id, &owner, &f.at.id, &flag, &(i64::MAX - 3)]).await.unwrap().get("item_id");
            place_sell(&Identity::of_client(&seller), &test_db, &seller.world, &seller.sde, SellRequest { item_id: item, station_id: f.at.id, type_id: f.type_id.into(), price: 1.0, quantity, duration: 1, office_id: None, expected_brokers_fee: None }, None).await.unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            buyer.world.register_pilot(f.issuer.character, tx);
            place_buy(&Identity::of_client(&buyer), &test_db, &buyer.world, &buyer.sde, BuyRequest { station_id: f.at.id, type_id: f.type_id.into(), price: 1.0, quantity, range: RANGE_STATION, min_volume: 1, duration: 0, expected_brokers_fee: None }, corporate.then_some(ACCOUNT_CASH + 1)).await.unwrap();
            let rows = test_db.query("SELECT item_id,quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 ORDER BY item_id", &[&owner, &f.at.id, &flag, &f.type_id]).await.unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows.iter().map(|r| i128::from(r.get::<_, i64>("quantity"))).sum::<i128>(), i128::from(i64::MAX - 3) + i128::from(quantity));
            assert!(rows.iter().any(|r| r.get::<_, i64>("item_id") == merged && r.get::<_, i64>("quantity") == i64::MAX));
            let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
            let notices: Vec<_> = events.iter().filter(|(name, _)| name == "OnMarketItemReceived").collect();
            assert_eq!(notices.len(), 2);
            for row in &rows { assert!(notices.iter().any(|(_, args)| args.first() == Some(&Value::Int(row.get("item_id"))))); }
            let fills = test_db.query("SELECT quantity FROM game.market_transactions WHERE buyer_id=$1 AND seller_id=$2 ORDER BY transaction_id", &[&f.issuer.character, &f.counterparty.character]).await.unwrap();
            assert_eq!(fills.len(), if corporate { 2 } else { 1 });
            assert_eq!(fills.last().unwrap().get::<_, i64>("quantity"), quantity);
            let event_count: i64 = test_db.query_one("SELECT count(*) AS n FROM game.progress_events WHERE kind='MarketOrderFilled' AND (payload->'MarketOrderFilled'->>'buyer_id')::bigint=$1 AND (payload->'MarketOrderFilled'->>'seller_id')::bigint=$2", &[&f.issuer.character, &f.counterparty.character]).await.unwrap().get("n");
            assert_eq!(event_count, if corporate { 2 } else { 1 });
            for (sequence, (name, args)) in events.iter().enumerate() {
                let value = Value::tuple(args.clone());
                let wire = marshal::dumps(&value);
                assert_eq!(marshal::loads(&wire).unwrap(), value);
                let wire_hex: String = wire.iter().map(|byte| format!("{byte:02x}")).collect();
                let after: Vec<_> = rows.iter().map(|row| serde_json::json!({
                    "item_id": row.get::<_, i64>("item_id").to_string(),
                    "quantity": row.get::<_, i64>("quantity").to_string()
                })).collect();
                println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
                    "version": 1, "id": format!("market-{}-{sequence}", if corporate { "corporate" } else { "personal" }),
                    "scenario": if corporate { "corporate_split_delivery" } else { "personal_split_delivery" },
                    "producer_test": "wide_order_fills_split_delivery_for_personal_and_corporate_buyers",
                    "producer_symbols": ["place_buy", "place_sell", "assets::deliver"],
                    "kind": "notification", "sequence": sequence, "recipient": f.issuer.character.to_string(),
                    "session": { "source": "fixture_context", "character_id": f.issuer.character.to_string(), "corporate": corporate, "station_id": f.at.id.to_string() },
                    "event": name, "payload_scope": "notification_args", "wire_hex": wire_hex, "wire_size": wire.len(),
                    "before": { "source": "fixture_context", "item_id": merged.to_string(), "quantity": (i64::MAX - 3).to_string(), "incoming": quantity.to_string() },
                    "after": { "source": "server_output", "rows": after }, "rows": []
                }));
            }
        }
    }).await;
    f.cleanup(&db).await;
    scenario.unwrap();
}
