//! Alliance row capital is projected from the same committed policy as sovereignty reads.
use super::super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL"]
async fn alliance_row_uses_current_capital_policy() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let result: Result<(), CallError> = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let row = tx.query_one("INSERT INTO game.alliances (name, short_name, creator_corp_id, creator_char_id, created) VALUES (gen_random_uuid()::text, gen_random_uuid()::text, 1, 1, 1) RETURNING alliance_id", &[]).await.map_err(db_err)?;
        let alliance: i64 = row.get("alliance_id");
        assert_eq!(alliance_line_of(tx, alliance).await?.unwrap()[14], Value::None);
        let mut policy = crate::dao::sovereignty::policy::lock(tx, alliance).await.map_err(db_err)?;
        policy.current_capital_system = Some(42);
        assert!(crate::dao::sovereignty::policy::replace(tx, policy.revision, &policy).await.map_err(db_err)?);
        assert_eq!(alliance_line_of(tx, alliance).await?.unwrap()[14], Value::Int(42));
        Err(CallError::Internal(anyhow::anyhow!("rollback test fixture")))
    }), |(), ()| async { Ok(()) }).await;
    assert!(matches!(result, Err(CallError::Internal(error)) if error.to_string() == "rollback test fixture"));
}
