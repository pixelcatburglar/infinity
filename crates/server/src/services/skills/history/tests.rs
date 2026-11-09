//! History ordering, limit and transaction rollback.
use super::*;
use crate::{db::{Db, outbox::Outbox}, services::tests::fixture};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn ordered_limited_and_rolled_back() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    history::append(&db, f.character, 1, GM_GIVE, 100, 10).await.unwrap();
    history::append(&db, f.character, 2, REMOVED, 200, 10).await.unwrap();
    history::append(&db, f.character, 3, TRAINING_COMPLETE, 300, 20).await.unwrap();
    let rows = history::list(&db, f.character, 2).await.unwrap();
    assert_eq!(rows.iter().map(|r| r.skill_type_id).collect::<Vec<_>>(), vec![3, 2]);
    let result: Result<(), CallError> = db.transaction(|tx, _outbox: &mut Outbox| Box::pin(async move {
        history::append(tx, f.character, 4, GM_GIVE, 400, 30).await.map_err(db_err)?;
        Err(CallError::Internal(anyhow::anyhow!("rollback")))
    }), |(), _outbox| async move { Ok(()) }).await;
    assert!(result.is_err());
    assert_eq!(history::list(&db, f.character, 50).await.unwrap().len(), 3);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn requested_count_controls_persisted_history() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    for i in 1..=55 {
        history::append(&db, f.character, i, GM_GIVE, i64::from(i) * 100, i64::from(i)).await.unwrap();
    }
    let (client, _peer) = crate::services::tests::client(db.clone(), f.character).await;
    let expected = |count: i32| Value::list((56 - count..=55).rev().map(|i| keyval([
        ("skillTypeID", Value::Int(i64::from(i))),
        ("eventTypeID", Value::Int(i64::from(GM_GIVE))),
        ("absolutePoints", Value::Int(i64::from(i) * 100)),
        ("logDate", Value::Int(i64::from(i))),
    ])));
    let default = get(&client, f.character, None).await.unwrap_or_else(|_| panic!("default history request failed"));
    assert_eq!(default, expected(50));
    let explicit = get(&client, f.character, Some(&Value::Int(51))).await.unwrap_or_else(|_| panic!("explicit history request failed"));
    assert_eq!(explicit, expected(51));
    let zero = get(&client, f.character, Some(&Value::Int(0))).await.unwrap_or_else(|_| panic!("zero history request failed"));
    assert_eq!(zero, Value::list([]));
    for invalid in [Value::Int(-1), Value::str("51"), Value::None, Value::Bool(true)] {
        match get(&client, f.character, Some(&invalid)).await {
            Err(CallError::User { msg, dict }) => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".into(), Value::unicode(messages::skills::INVALID_HISTORY_REQUEST))]);
            }
            _ => panic!("invalid history request was not refused"),
        }
    }
    let unchanged = get(&client, f.character, Some(&Value::Int(55))).await.unwrap_or_else(|_| panic!("history request after refusals failed"));
    assert_eq!(unchanged, expected(55));
    fixture::cleanup(&db, f).await.unwrap();
    let remaining: i64 = db.query_one("SELECT count(*) AS count FROM game.skill_history WHERE character_id = $1", &[&f.character]).await.unwrap().get("count");
    assert_eq!(remaining, 0);
}
