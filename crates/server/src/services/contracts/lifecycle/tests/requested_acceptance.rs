//! Dedicated database regressions for exchange acceptance.
use super::super::*;

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn requested_acceptance_preserves_items_and_rolls_back_failed_settlement() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let issuer = fixture::create(&db).await.unwrap();
    let buyer = fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), buyer.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let blueprint = client.sde.types.iter().find(|t| t.category_id == Some(9)).unwrap().type_id;
    let stack_type = client.sde.types.iter().find(|t| t.type_id > 0 && t.category_id == Some(crate::consts::CATEGORY_CHARGE) && t.published == Some(true)).expect("published charge type").type_id;
    let root = transfers::insert(&db, blueprint, buyer.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true, quantity = -2 WHERE item_id = $1", &[&root]).await.unwrap();
    db.execute("INSERT INTO game.blueprints (item_id, material_efficiency, time_efficiency, runs) VALUES ($1, 7, 9, 3)", &[&root]).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&db, root, 1, 42.0).await.unwrap());
    let child = transfers::insert(&db, stack_type, buyer.character, root, FLAG_HANGAR, 1).await.unwrap();
    let stack = transfers::insert(&db, stack_type, buyer.character, station, FLAG_HANGAR, 10).await.unwrap();
    db.execute("UPDATE game.items SET custom_info = 'metadata', name = 'named' WHERE item_id = $1", &[&stack]).await.unwrap();
    // The issuer starts at the exact maximum for the upgraded numeric(36,2) balance.
    db.execute("UPDATE game.characters SET balance = CASE WHEN character_id = $1 THEN 9999999999999999999999999999999999.99 ELSE 100 END WHERE character_id = ANY($2)", &[&issuer.character, &vec![issuer.character, buyer.character]]).await.unwrap();
    let id = contract_dao::insert(&db, &contract_dao::NewContract {
        kind: TYPE_ITEM_EXCHANGE, issuer: issuer.character, issuer_corp: 1, assignee: 0, start: station, end: 0,
        price: 1.0, reward: 0.0, collateral: 0.0, title: "", description: "", num_days: 0,
        paid_deposit: 0.0, auction_tax_rate: 0.0, auction_tax_maximum: 0.0,
        status: STATUS_OUTSTANDING, issued: 0, expires: i64::MAX,
        for_corp: false, issuer_wallet_key: ACCOUNT_CASH, issuer_hangar_flag: FLAG_HANGAR, issuer_source_location: station,
    }).await.unwrap();
    contract_dao::insert_requested_item(&db, id, blueprint, 1).await.unwrap();
    contract_dao::insert_requested_item(&db, id, stack_type, 3).await.unwrap();
    let duplicate_payload = Value::Dict(vec![
        (Value::str("contractType"), Value::Int(i64::from(TYPE_ITEM_EXCHANGE))),
        (Value::str("startStationID"), Value::Int(station)),
        (Value::str("requestItemTypeList"), Value::list([
            Value::tuple([Value::Int(i64::from(blueprint)), Value::Int(1)]),
            Value::tuple([Value::Int(i64::from(blueprint)), Value::Int(1)]),
        ])),
    ]);
    let duplicate_create = create(&client, &duplicate_payload).await;
    contract_dao::insert_requested_item(&db, id, blueprint, 1).await.unwrap();
    let duplicate_accept = accept(&client, id, false).await;
    db.execute("DELETE FROM game.contract_items WHERE ctid IN (SELECT ctid FROM game.contract_items WHERE contract_id = $1 AND type_id = $2 LIMIT 1)", &[&id, &blueprint]).await.unwrap();
    db.execute("UPDATE game.contract_items SET quantity = 11 WHERE contract_id = $1 AND type_id = $2", &[&id, &stack_type]).await.unwrap();
    let insufficient = accept(&client, id, false).await;
    db.execute("UPDATE game.contract_items SET quantity = 3 WHERE contract_id = $1 AND type_id = $2", &[&id, &stack_type]).await.unwrap();
    let mut reserved = Vec::new();
    for trade_id in [root, child] {
        db.execute("INSERT INTO game.trades (trade_id, station_id, char_a, char_b, state, created_at, updated_at) VALUES ($1, $2, $3, $4, 'open', 0, 0)", &[&trade_id, &station, &buyer.character, &issuer.character]).await.unwrap();
        reserved.push(accept(&client, id, false).await.is_err());
        db.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&trade_id]).await.unwrap();
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(buyer.character, tx);
    // The issuer wallet overflows only after the real requested-item transfers.
    let failure = accept(&client, id, false).await;
    let unchanged = db.query_one("SELECT (SELECT owner_id FROM game.items WHERE item_id = $1) AS owner, (SELECT quantity FROM game.items WHERE item_id = $2) AS quantity, (SELECT status FROM game.contracts WHERE contract_id = $3) AS status, (SELECT balance::float8 FROM game.characters WHERE character_id = $4) AS balance", &[&root, &stack, &id, &buyer.character]).await;
    let quiet = rx.try_recv().is_err();
    db.execute("UPDATE game.characters SET balance = 0 WHERE character_id = $1", &[&issuer.character]).await.unwrap();
    let success = accept(&client, id, false).await;
    let row = db.query_one("SELECT i.owner_id, i.quantity, i.singleton, b.material_efficiency, b.time_efficiency, b.runs, a.value FROM game.items i JOIN game.blueprints b USING (item_id) JOIN game.item_attributes a USING (item_id) WHERE i.item_id = $1 AND a.attribute_id = 1", &[&root]).await;
    let descendant = db.query_one("SELECT owner_id, location_id FROM game.items WHERE item_id = $1", &[&child]).await;
    let split = db.query_one("SELECT quantity, custom_info, name FROM game.items WHERE owner_id = $1 AND type_id = $2 AND location_id = $3", &[&issuer.character, &stack_type, &station]).await;
    let remaining = db.query_one("SELECT quantity FROM game.items WHERE item_id = $1", &[&stack]).await;
    let status = db.query_one("SELECT status FROM game.contracts WHERE contract_id = $1", &[&id]).await;
    client.world.unregister_pilot(buyer.character);
    db.execute("DELETE FROM game.contract_items WHERE contract_id = $1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.contracts WHERE contract_id = $1", &[&id]).await.unwrap();
    db.execute("DELETE FROM game.blueprints WHERE item_id = $1", &[&root]).await.unwrap();
    fixture::cleanup(&db, buyer).await.unwrap();
    fixture::cleanup(&db, issuer).await.unwrap();
    if let Err(error) = &success {
        let detail = match error {
            CallError::Internal(e) => format!("{e:#}"),
            CallError::User { msg, dict } => format!("{msg} {dict:?}"),
        };
        panic!("requested-item acceptance failed: {detail}");
    }
    match &failure {
        Err(CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
            Some(crate::db::Error::Postgres(error)) if error.code() == Some(&tokio_postgres::error::SqlState::NUMERIC_VALUE_OUT_OF_RANGE) => {}
            _ => panic!("expected PostgreSQL numeric overflow after transfers, got {error:#}"),
        },
        Err(CallError::User { msg, dict }) => panic!("expected PostgreSQL numeric overflow after transfers, got {msg} {dict:?}"),
        Ok(_) => panic!("expected PostgreSQL numeric overflow after transfers, acceptance succeeded"),
    }
    let unchanged = unchanged.unwrap();
    let row = row.unwrap();
    let descendant = descendant.unwrap();
    let split = split.unwrap();
    let remaining: i32 = remaining.unwrap().get("quantity");
    let status: i32 = status.unwrap().get("status");
    assert_eq!(unchanged.get::<_, i64>("owner"), buyer.character);
    assert_eq!(unchanged.get::<_, i32>("quantity"), 10);
    assert_eq!(unchanged.get::<_, i32>("status"), STATUS_OUTSTANDING);
    assert_eq!(unchanged.get::<_, f64>("balance"), 100.0);
    assert!(quiet);
    assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i32>("quantity"), row.get::<_, bool>("singleton")), (issuer.character, -2, true));
    assert_eq!((row.get::<_, i32>("material_efficiency"), row.get::<_, i32>("time_efficiency"), row.get::<_, i32>("runs")), (7, 9, 3));
    assert_eq!(row.get::<_, f64>("value"), 42.0);
    assert!(duplicate_create.is_err());
    assert!(duplicate_accept.is_err());
    assert!(insufficient.is_err());
    assert_eq!(reserved, vec![true, true]);
    assert_eq!((descendant.get::<_, i64>("owner_id"), descendant.get::<_, i64>("location_id")), (issuer.character, root));
    assert_eq!((split.get::<_, i32>("quantity"), split.get::<_, Option<String>>("custom_info").as_deref(), split.get::<_, Option<String>>("name").as_deref()), (3, Some("metadata"), Some("named")));
    assert_eq!(remaining, 7);
    assert_eq!(status, STATUS_FINISHED);
}
