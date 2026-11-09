//! Database regressions for saved fitting route atomicity.
use super::*;
use marshal::Value;
use tokio_postgres::error::SqlState;

fn fitting(name: &str) -> Value {
    keyval([
        ("name", Value::unicode(name)),
        ("description", Value::unicode("database-test")),
        ("shipTypeID", Value::Int(587)),
        ("fitData", Value::list([Value::list([Value::Int(34), Value::Int(5), Value::Int(1)])])),
    ])
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn saved_fitting_route_rolls_back_when_a_later_insert_fails() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let constraint = format!("saved_fitting_route_{}_name_check", fixture.character);
    let result = crate::tests::catch_scenario(async {
    let sentinel = crate::dao::items::fitting::save(&db, fixture.character, "sentinel", "unchanged", 587, &[34], &[5], &[1]).await.unwrap();
    db.execute(
        &format!("ALTER TABLE game.fittings ADD CONSTRAINT {constraint} CHECK (owner_id <> {} OR name <> 'database-failure') NOT VALID", fixture.character),
        &[],
    ).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let preflight = dispatch_save(&mut client, fixture.character, vec![
        (Value::str("first"), fitting("database-failure")),
        (Value::str("second"), named_fitting("invalid", "database-test", Value::list([
            row(Value::Int(34), Value::Int(5), Value::Int(1)),
            row(Value::Int(34), Value::Int(5), Value::Int(0)),
        ]))),
    ]).await;
    assert!(is_custom_notify(&preflight));
    let rows = db.query("SELECT fitting_id,name FROM game.fittings WHERE owner_id=$1 ORDER BY fitting_id", &[&fixture.character]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, i64>("fitting_id"), sentinel);
    assert_eq!(rows[0].get::<_, String>("name"), "sentinel");
    let args = [
        Value::Int(fixture.character),
        Value::Dict(vec![(Value::str("first"), fitting("database-first")), (Value::str("second"), fitting("database-failure"))]),
    ];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "charFittingMgr", method: "SaveManyFittings", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let result = crate::services::dispatch(&mut client, &call).await;
    let check_violation = matches!(result, Err(CallError::Internal(error)) if matches!(error.downcast_ref::<crate::db::Error>(), Some(crate::db::Error::Postgres(postgres)) if postgres.code() == Some(&SqlState::CHECK_VIOLATION)));
    let rows = db.query("SELECT fitting_id,name FROM game.fittings WHERE owner_id=$1 ORDER BY fitting_id", &[&fixture.character]).await.unwrap();
    let unchanged = rows.len() == 1
        && rows[0].get::<_, i64>("fitting_id") == sentinel
        && rows[0].get::<_, String>("name") == "sentinel";
    assert!(check_violation);
    assert!(unchanged);
    }).await;
    let drop_constraint = db.execute(&format!("ALTER TABLE game.fittings DROP CONSTRAINT IF EXISTS {constraint}"), &[]).await;
    let delete = db.execute("DELETE FROM game.fittings WHERE owner_id=$1", &[&fixture.character]).await;
    let cleanup = crate::services::tests::fixture::cleanup(&db, fixture).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    drop_constraint.unwrap();
    delete.unwrap();
    cleanup.unwrap();
}

 
