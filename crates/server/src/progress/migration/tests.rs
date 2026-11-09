use super::*;

#[test]
fn legacy_wallet_validation_and_other_payloads() {
    let input = r#"{"WalletChanged":{"owner_id":1,"account_key":2,"balance":12.345,"delta":-2.5}}"#;
    let value: serde_json::Value = serde_json::from_str(&convert(1, "WalletChanged", input).unwrap()).unwrap();
    assert_eq!(value["WalletChanged"]["balance"], IskAmount::from_isk(12.345).unwrap().decimal());
    assert_eq!(value["WalletChanged"]["delta"], "-2.50");
    for invalid in ["null", "\"NaN\"", "1e35", "\"1.00\""] {
        let payload = format!(r#"{{"WalletChanged":{{"owner_id":1,"account_key":2,"balance":{invalid},"delta":0}}}}"#);
        assert!(convert(1, "WalletChanged", &payload).is_err());
    }
    assert!(IskAmount::from_isk(f64::NAN).is_err());
    assert!(IskAmount::from_isk(f64::INFINITY).is_err());
    let other = r#"{"MissionCompleted":{"char_id":1,"agent_id":2,"mission_id":3}}"#;
    for version in [1, 2] {
        assert_eq!(serde_json::from_str::<serde_json::Value>(&convert(version, "MissionCompleted", other).unwrap()).unwrap(), serde_json::from_str::<serde_json::Value>(other).unwrap());
        assert!(convert(version, "MissionAccepted", other).is_err());
    }
    let wallet_v2 = r#"{"WalletChanged":{"owner_id":1,"account_key":2,"balance":"12.34","delta":"-0.01"}}"#;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&convert(2, "WalletChanged", wallet_v2).unwrap()).unwrap(), serde_json::from_str::<serde_json::Value>(wallet_v2).unwrap());
    for (kind, payload) in [
        ("MarketOrderPlaced", r#"{"MarketOrderPlaced":{"char_id":1,"order_id":2,"type_id":3,"quantity":4,"price":12.5,"is_buy":true}}"#),
        ("MarketOrderFilled", r#"{"MarketOrderFilled":{"order_id":1,"buyer_id":2,"seller_id":3,"type_id":4,"quantity":5,"price":12.5}}"#),
    ] {
        for version in [1, 2] {
            let value: serde_json::Value = serde_json::from_str(&convert(version, kind, payload).unwrap()).unwrap();
            assert_eq!(value[kind]["price"], "12.50");
            let bad = payload.replace("12.5", "null");
            assert!(convert(version, kind, &bad).is_err());
        }
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn migration_preserves_receipts_and_rolls_back_invalid_rows() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let namespace = format!("migration-test-{}", crate::handshake::random_u64());
    let applied = format!("{namespace}-applied");
    let pending = format!("{namespace}-pending");
    for key in [&applied, &pending] {
        tx.execute("INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,10,1,true)", &[key]).await.unwrap();
    }
    let wallet = r#"{"WalletChanged":{"owner_id":1,"account_key":2,"balance":12.5,"delta":-0.01}}"#;
    let other = r#"{"MissionCompleted":{"char_id":1,"agent_id":2,"mission_id":3}}"#;
    let market = r#"{"MarketOrderFilled":{"order_id":1,"buyer_id":2,"seller_id":3,"type_id":4,"quantity":5,"price":2.5}}"#;
    for (key, version, kind, payload) in [("wallet", 1_i32, "WalletChanged", wallet), ("other", 2, "MissionCompleted", other), ("market", 2, "MarketOrderFilled", market)] {
        tx.execute("INSERT INTO game.progress_events (source_namespace,source_key,schema_version,kind,occurred_at,recorded_at,payload,context,causation,origin,ordinal,subject_character_id) VALUES ($1,$2,$3,$4,11,12,$5::text::jsonb,'{\"x\":1}'::jsonb,'cause','test',3,42)", &[&namespace, &key, &version, &kind, &payload]).await.unwrap();
    }
    for (key, state, at) in [(&applied, "applied", Some(55_i64)), (&pending, "pending", None)] {
        tx.execute("INSERT INTO game.progress_deliveries (consumer_key,event_id,state,applied_at,next_attempt_at,attempts,last_error,subject_character_id) SELECT $1,event_id,$2,$3,99,4,'retry',42 FROM game.progress_events WHERE source_namespace=$4 AND source_key='wallet'", &[key, &state, &at, &namespace]).await.unwrap();
    }
    let snapshot = |row: &tokio_postgres::Row| row.get::<_, String>("snapshot");
    let before = tx.query_one("SELECT jsonb_agg(to_jsonb(e) - 'schema_version' - 'payload' ORDER BY source_key)::text AS snapshot FROM game.progress_events e WHERE source_namespace=$1", &[&namespace]).await.unwrap();
    let receipts = tx.query_one("SELECT jsonb_agg(to_jsonb(d) ORDER BY consumer_key)::text AS snapshot FROM game.progress_deliveries d JOIN game.progress_events e USING(event_id) WHERE e.source_namespace=$1", &[&namespace]).await.unwrap();
    {
        let save = tx.savepoint("invalid_migration").await.unwrap();
        save.execute("INSERT INTO game.progress_events (source_namespace,source_key,schema_version,kind,occurred_at,recorded_at,payload,context,origin,ordinal) VALUES ($1,'invalid',1,'WalletChanged',1,1,'{\"WalletChanged\":{\"owner_id\":1,\"account_key\":2,\"balance\":null,\"delta\":0}}','{}','test',0)", &[&namespace]).await.unwrap();
        assert!(migrate_tx(&save).await.is_err());
        save.rollback().await.unwrap();
    }
    assert_eq!(tx.query_one("SELECT count(*) AS n FROM game.progress_events WHERE source_namespace=$1 AND schema_version IN (1,2)", &[&namespace]).await.unwrap().get::<_, i64>("n"), 3);
    migrate_tx(&tx).await.unwrap();
    assert_eq!(migrate_tx(&tx).await.unwrap(), 0);
    let after = tx.query_one("SELECT jsonb_agg(to_jsonb(e) - 'schema_version' - 'payload' ORDER BY source_key)::text AS snapshot FROM game.progress_events e WHERE source_namespace=$1", &[&namespace]).await.unwrap();
    let receipts_after = tx.query_one("SELECT jsonb_agg(to_jsonb(d) ORDER BY consumer_key)::text AS snapshot FROM game.progress_deliveries d JOIN game.progress_events e USING(event_id) WHERE e.source_namespace=$1", &[&namespace]).await.unwrap();
    assert_eq!(snapshot(&before), snapshot(&after));
    assert_eq!(snapshot(&receipts), snapshot(&receipts_after));
    let event_id: String = tx.query_one("SELECT event_id::text AS id FROM game.progress_events WHERE source_namespace=$1 AND source_key='wallet'", &[&namespace]).await.unwrap().get("id");
    let applied_key = crate::progress::DeliveryKey { consumer_key: applied.clone(), event_id: event_id.clone() };
    let pending_key = crate::progress::DeliveryKey { consumer_key: pending.clone(), event_id };
    assert!(crate::dao::progress::lock_pending(&tx, &applied_key).await.unwrap().is_none());
    assert!(crate::dao::progress::lock_pending(&tx, &pending_key).await.unwrap().is_some());
    let rows = tx.query("SELECT schema_version,kind,payload::text AS payload FROM game.progress_events WHERE source_namespace=$1 ORDER BY source_key", &[&namespace]).await.unwrap();
    for row in rows {
        let kind: String = row.get("kind");
        let payload: String = row.get("payload");
        assert_eq!(row.get::<_, i32>("schema_version"), codec::SCHEMA_VERSION);
        codec::decode(codec::SCHEMA_VERSION, &kind, &payload).unwrap();
        if kind == "MissionCompleted" { assert_eq!(serde_json::from_str::<serde_json::Value>(&payload).unwrap(), serde_json::from_str::<serde_json::Value>(other).unwrap()); }
    }
    tx.rollback().await.unwrap();
}
