//! Bill DAO regression tests: exact numeric(36,2) amounts, typed bill decode and
//! the guarded settlement primitives.

use super::*;


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn bill_amount_decodes_exact_numeric_text() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let bill: i64 = db.query_one("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, created) VALUES (2, $1, $1, '1234567890123456789012345678901234.56'::numeric, 0, 0) RETURNING bill_id", &[&fixture.character]).await.unwrap().get("bill_id");
    let row = get(&db, bill).await.unwrap().expect("inserted bill");
    assert_eq!(row.amount.decimal(), "1234567890123456789012345678901234.56");
    assert_eq!(row.status, BILL_STATUS_UNPAID);
    assert_eq!((row.period_start, row.period_end), (None, None));
    db.execute("DELETE FROM game.bills WHERE bill_id = $1", &[&bill]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn claim_paid_is_guarded_on_every_bill_fact() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let bill: i64 = db.query_one("INSERT INTO game.bills (bill_type, debtor_id, creditor_id, amount, due, created, period_start, period_end) VALUES (2, $1, $1, 100.00, 10, 0, 10, 40) RETURNING bill_id", &[&fixture.character]).await.unwrap().get("bill_id");
    let row = get(&db, bill).await.unwrap().expect("inserted bill");
    // A mismatched expectation refuses without touching the row.
    let stale = Bill { due: 11, ..row.clone() };
    assert!(!claim_paid(&db, &stale, 20).await.unwrap());
    assert_eq!(get(&db, bill).await.unwrap().unwrap().status, BILL_STATUS_UNPAID);
    assert!(claim_paid(&db, &row, 20).await.unwrap());
    let settled = get(&db, bill).await.unwrap().unwrap();
    assert_eq!(settled.status, BILL_STATUS_PAID);
    let settled_at: Option<i64> = db.query_one("SELECT settled_at FROM game.bills WHERE bill_id=$1", &[&bill]).await.unwrap().get("settled_at");
    assert_eq!(settled_at, Some(20));
    // A second claim on the settled bill cannot re-pay it.
    assert!(!claim_paid(&db, &row, 21).await.unwrap());
    db.execute("DELETE FROM game.bills WHERE bill_id = $1", &[&bill]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn legacy_period_migration_preserves_paid_history_and_is_repeatable() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let office: i64 = db.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let station = 1_i64;
    let until = 100_i64;
    crate::dao::offices::insert(&db, office, actor.character, station, until, 0).await.unwrap();
    let insert = "INSERT INTO game.bills (bill_type,debtor_id,creditor_id,amount,due,created,external_id,external_id2,status) VALUES (2,$1,1,123.45,$2,0,$3,$4,$5) RETURNING bill_id";
    let first: i64 = db.query_one(insert, &[&actor.character, &until, &Some(office), &Some(station), &0_i32]).await.unwrap().get("bill_id");
    let duplicate: i64 = db.query_one(insert, &[&actor.character, &until, &Some(office), &Some(station), &0_i32]).await.unwrap().get("bill_id");
    let missing: i64 = db.query_one(insert, &[&actor.character, &until, &Option::<i64>::None, &Some(station), &0_i32]).await.unwrap().get("bill_id");
    let paid: i64 = db.query_one(insert, &[&actor.character, &until, &Some(office), &Some(station), &1_i32]).await.unwrap().get("bill_id");
    for _ in 0..2 {
        let restarted = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: true, ..config.clone() }).await.unwrap();
        let first_row = get(&restarted, first).await.unwrap().unwrap();
        assert_eq!((first_row.status, first_row.period_start, first_row.amount.decimal().as_str()), (BILL_STATUS_UNPAID, Some(until), "123.45"));
        assert_eq!(get(&restarted, duplicate).await.unwrap().unwrap().status, BILL_STATUS_CANCELLED);
        assert_eq!(get(&restarted, missing).await.unwrap().unwrap().status, BILL_STATUS_CANCELLED);
        let paid_row = get(&restarted, paid).await.unwrap().unwrap();
        assert_eq!((paid_row.status, paid_row.period_start, paid_row.amount.decimal().as_str()), (BILL_STATUS_PAID, None, "123.45"));
    }
    db.execute("DELETE FROM game.bills WHERE bill_id=ANY($1)", &[&vec![first, duplicate, missing, paid]]).await.unwrap();
    db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&office]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
}
