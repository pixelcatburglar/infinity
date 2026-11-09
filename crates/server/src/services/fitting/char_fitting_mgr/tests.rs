//! Route regressions for saved fitting input validation and batch atomicity.
use super::*;
use marshal::Value;

mod database;

fn fitting(fit_data: Value) -> Value {
    keyval([
        ("name", Value::unicode("route-test")),
        ("description", Value::unicode("route-test-description")),
        ("shipTypeID", Value::Int(587)),
        ("fitData", fit_data),
    ])
}

fn row(t: Value, f: Value, q: Value) -> Value {
    Value::list([t, f, q])
}
fn named_fitting(name: &str, description: &str, fit_data: Value) -> Value {
    keyval([
        ("name", Value::unicode(name)),
        ("description", Value::unicode(description)),
        ("shipTypeID", Value::Int(587)),
        ("fitData", fit_data),
    ])
}

async fn dispatch_method(client: &mut crate::client::Client, method: &str, args: Vec<Value>) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "charFittingMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(client, &call).await
}


async fn dispatch_save(client: &mut crate::client::Client, owner: i64, fits: Vec<(Value, Value)>) -> CallResult {
    let args = [Value::Int(owner), Value::Dict(fits)];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "charFittingMgr", method: "SaveManyFittings", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(client, &call).await
}

async fn dispatch_args(client: &mut crate::client::Client, args: Vec<Value>) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "charFittingMgr", method: "SaveManyFittings", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(client, &call).await
}

fn is_custom_notify(result: &CallResult) -> bool {
    matches!(result, Err(CallError::User { msg, dict }) if *msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict.iter().any(|(key, _)| key == "notify"))
}

#[test]
fn parse_fit_data_enforces_integer_triples_and_positive_i64_quantity() {
    let valid = parse_fit_data(Some(&Value::list([
        row(Value::Int(i64::from(i32::MIN)), Value::Int(0), Value::Int(1)),
        row(Value::Int(0), Value::Int(i64::from(i32::MAX)), Value::Int(i64::from(i32::MAX))),
    ]))).unwrap();
    assert_eq!(valid.types, [i32::MIN, 0]);
    assert_eq!(valid.flags, [0, i32::MAX]);
    assert_eq!(valid.qty, [1, i64::from(i32::MAX)]);
    for value in [
        Value::None,
        Value::Float(1.0),
        Value::list([Value::list([Value::Int(1), Value::Int(2)])]),
        Value::list([Value::list([Value::Int(1), Value::Int(2), Value::Int(0)])]),
        Value::list([Value::list([Value::Int(i64::from(i32::MAX) + 1), Value::Int(2), Value::Int(1)])]),
    ] {
        assert!(parse_fit_data(Some(&value)).is_err());
    }
    assert!(parse_fit_data(None).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn saved_fitting_route_rejects_invalid_quantities_and_shapes() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(async {
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let mut outcomes = Vec::new();
    let invalid_quantities = [
        Value::Int(-1), Value::Int(0), Value::None,
        Value::Float(1.5), Value::Bytes(b"2".to_vec()), Value::Bool(true),
        Value::Float(1.0), Value::unicode("2"), Value::BigLong(vec![0, 0, 0, 0, 0, 0, 0, 0x80, 0]),
    ];
    for quantity in invalid_quantities {
        let fit = fitting(Value::list([row(Value::Int(34), Value::Int(5), quantity)]));
        outcomes.push(is_custom_notify(&dispatch_save(&mut client, fixture.character, vec![(Value::str("bad"), fit)]).await));
    }
    for (t, f) in [
        (Value::None, Value::Int(5)),
        (Value::Float(1.0), Value::Int(5)),
        (Value::Int(i64::from(i32::MAX) + 1), Value::Int(5)),
        (Value::Int(34), Value::None),
        (Value::Int(34), Value::Float(1.0)),
        (Value::Int(34), Value::Int(i64::from(i32::MAX) + 1)),
    ] {
        let fit = fitting(Value::list([row(t, f, Value::Int(1))]));
        outcomes.push(is_custom_notify(&dispatch_save(&mut client, fixture.character, vec![(Value::str("bad"), fit)]).await));
    }
    for data in [
        Value::list([Value::list([])]),
        Value::list([Value::list([Value::Int(34)])]),
        Value::list([Value::list([Value::Int(34), Value::Int(5), Value::Int(1), Value::Int(2)])]),
        Value::list([Value::Int(34)]),
        Value::list([Value::None]),
        Value::None,
    ] {
        let fit = fitting(data);
        outcomes.push(is_custom_notify(&dispatch_save(&mut client, fixture.character, vec![(Value::str("bad"), fit)]).await));
    }
    let missing_fit_data = keyval([
        ("name", Value::unicode("route-test")),
        ("description", Value::unicode("route-test-description")),
        ("shipTypeID", Value::Int(587)),
    ]);
    outcomes.push(is_custom_notify(&dispatch_save(&mut client, fixture.character, vec![(Value::str("missing"), missing_fit_data)]).await));
    outcomes.push(is_custom_notify(&dispatch_save(&mut client, fixture.character, vec![(Value::str("scalar"), Value::Int(1))]).await));
    outcomes.push(is_custom_notify(&dispatch_args(&mut client, vec![Value::Int(fixture.character)]).await));
    outcomes.push(is_custom_notify(&dispatch_args(&mut client, vec![Value::Int(fixture.character), Value::None]).await));
    let fitting_count: i64 = db.query_one("SELECT count(*) AS count FROM game.fittings WHERE owner_id=$1", &[&fixture.character]).await.unwrap().get("count");
    let valid = fitting(Value::list([
        row(Value::Int(i64::from(i32::MIN)), Value::Int(0), Value::Int(1)),
        row(Value::Int(i64::from(i32::MAX)), Value::Int(i64::from(i32::MAX)), Value::Int(i64::from(i32::MAX))),
    ]));
    let valid_result = dispatch_save(&mut client, fixture.character, vec![(Value::str("valid"), valid)]).await;
    let valid_count: i64 = db.query_one("SELECT count(*) AS count FROM game.fittings WHERE owner_id=$1", &[&fixture.character]).await.unwrap().get("count");
    assert!(outcomes.into_iter().all(|outcome| outcome));
    assert_eq!(fitting_count, 0);
    assert!(matches!(valid_result, Ok(Value::List(rows)) if rows.len() == 1));
    assert_eq!(valid_count, 1);
    }).await;
    let delete = db.execute("DELETE FROM game.fittings WHERE owner_id=$1", &[&fixture.character]).await;
    let cleanup = crate::services::tests::fixture::cleanup(&db, fixture).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    delete.unwrap();
    cleanup.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn saved_fitting_route_preserves_batch_order_and_wire_shape() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    let result = crate::tests::catch_scenario(async {
    let quantities = [1_i64, i64::from(i32::MAX), 1_i64 << 31, 1_i64 << 32, (1_i64 << 53) + 1, i64::MAX];
    let first = named_fitting("first", "first-description", Value::list(
        quantities.iter().map(|&q| row(Value::Int(0), Value::Int(i64::from(i32::MAX)), Value::Int(q))),
    ));
    let second = named_fitting("second", "second-description", Value::tuple([
        row(Value::Int(i32::MAX.into()), Value::Int(i32::MIN.into()), Value::Int(1)),
    ]));
    let saved = dispatch_save(&mut client, owner.character, vec![
        (Value::str("temp-first"), first),
        (Value::str("temp-second"), second),
    ]).await.unwrap();
    let Value::List(mappings) = &saved else { panic!("expected fitting mappings"); };
    assert_eq!(mappings.len(), 2);
    assert_eq!(value_field(&mappings[0], "tempFittingID"), Some(&Value::str("temp-first")));
    assert_eq!(value_field(&mappings[1], "tempFittingID"), Some(&Value::str("temp-second")));
    let listed_value = dispatch_method(&mut client, "GetFittings", vec![Value::Int(owner.character)]).await.unwrap();
    let listed_wire = marshal::dumps(&listed_value);
    let listed = marshal::loads(&listed_wire).unwrap();
    assert_eq!(marshal::dumps(&listed), listed_wire);
    let Value::Dict(rows) = &listed else { panic!("expected fitting dictionary"); };
    let first_id = value_field(&mappings[0], "realFittingID").and_then(Value::as_int).expect("real fitting id");
    let stored = fitting_dao::fittings(&db, owner.character).await.unwrap();
    assert_eq!(stored.iter().find(|f| f.fitting_id == first_id).unwrap().fit_qty, quantities);
    assert_eq!(rows[0].0.as_int(), Some(first_id));
    assert_eq!(value_field(&rows[0].1, "fittingID"), Some(&Value::Int(first_id)));
    assert_eq!(rows.len(), 2);
    assert_eq!(value_field(&rows[0].1, "name"), Some(&Value::unicode("first")));
    assert_eq!(value_field(&rows[0].1, "description"), Some(&Value::unicode("first-description")));
    assert_eq!(value_field(&rows[0].1, "shipTypeID"), Some(&Value::Int(587)));
    let Some(Value::List(first_rows)) = value_field(&rows[0].1, "fitData") else { panic!("expected first fitData"); };
    assert_eq!(first_rows.len(), quantities.len());
    for (row, &quantity) in first_rows.iter().zip(&quantities) {
        assert_eq!(*row, Value::tuple([Value::Int(0), Value::Int(i64::from(i32::MAX)), Value::Int(quantity.min(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT))]));
    }
    let scalar = |value: &Value| match value {
        Value::Int(value) => serde_json::json!({"type":"int","value":value.to_string()}),
        Value::Unicode(value) => serde_json::json!({"type":"unicode","value":value}),
        _ => panic!("unexpected saved fitting scalar"),
    };
    let fit_data = first_rows.iter().map(|row| {
        let Value::Tuple(values) = row else { panic!("expected fitting tuple"); };
        assert_eq!(values.len(), 3);
        values.iter().map(scalar).collect::<Vec<_>>()
    }).collect::<Vec<_>>();
    println!("CLIENT_QUANTITY_INPUT_V1 {}", serde_json::json!({
        "version":1,
        "id":"saved-fitting-wide-quantities",
        "scenario":"saved_fitting_get_fittings",
        "producer_test":"saved_fitting_route_preserves_batch_order_and_wire_shape",
        "producer_symbols":["charFittingMgr::SaveManyFittings","charFittingMgr::GetFittings","fitting_value","marshal::dumps","marshal::loads"],
        "kind":"rpc_reply",
        "sequence":0,
        "recipient":{"kind":"direct_client","character_id":owner.character.to_string()},
        "session":{"source":"fixture_context","character_id":owner.character.to_string()},
        "wire_hex":listed_wire.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
        "wire_size":listed_wire.len(),
        "payload_scope":"ordinary_response",
        "rows":[],
        "saved_fit_data":[{
            "fitting_id":scalar(value_field(&rows[0].1, "fittingID").unwrap()),
            "owner_id":scalar(value_field(&rows[0].1, "ownerID").unwrap()),
            "name":scalar(value_field(&rows[0].1, "name").unwrap()),
            "description":scalar(value_field(&rows[0].1, "description").unwrap()),
            "ship_type_id":scalar(value_field(&rows[0].1, "shipTypeID").unwrap()),
            "fit_data":fit_data
        }]
    }));
    let empty_list = dispatch_save(&mut client, owner.character, vec![(Value::str("empty-list"), named_fitting("empty-list", "", Value::list([])))]).await.unwrap();
    assert!(matches!(empty_list, Value::List(ref values) if values.len() == 1));
    let empty_tuple = dispatch_save(&mut client, owner.character, vec![(Value::str("empty-tuple"), named_fitting("empty-tuple", "", Value::tuple([])))]).await.unwrap();
    assert!(matches!(empty_tuple, Value::List(ref values) if values.len() == 1));
    let before_atomic: i64 = db.query_one("SELECT count(*) AS count FROM game.fittings WHERE owner_id=$1", &[&owner.character]).await.unwrap().get("count");
    let atomic = dispatch_save(&mut client, owner.character, vec![
        (Value::str("atomic-first"), named_fitting("atomic-first", "", Value::list([row(Value::Int(34), Value::Int(5), Value::Int(1))]))),
        (Value::str("atomic-bad"), named_fitting("atomic-bad", "", Value::list([Value::list([Value::Int(34), Value::Int(5)])]))),
    ]).await;
    assert!(is_custom_notify(&atomic));
    let after_atomic: i64 = db.query_one("SELECT count(*) AS count FROM game.fittings WHERE owner_id=$1", &[&owner.character]).await.unwrap().get("count");
    assert_eq!(after_atomic, before_atomic);
    assert!(matches!(dispatch_save(&mut client, owner.character, vec![]).await, Ok(Value::List(values)) if values.is_empty()));
    assert!(matches!(dispatch_save(&mut client, other.character, vec![(Value::str("foreign"), named_fitting("foreign", "", Value::list([])))]).await, Ok(Value::None)));
    assert!(matches!(dispatch_method(&mut client, "GetFittings", vec![Value::Int(other.character)]).await, Ok(Value::Dict(values)) if values.is_empty()));
    }).await;
    db.execute("DELETE FROM game.fittings WHERE owner_id=ANY($1)", &[&vec![owner.character, other.character]]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}
