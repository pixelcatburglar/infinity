use super::*;

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn capacity_reply_counts_packaged_and_assembled_rifters() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let rifter = sde.types.get(587).expect("Rifter in SDE");
    let assembled_volume = rifter.volume.unwrap();
    assert!(assembled_volume > 2500.0);
    let hold_type = sde.types.iter().find(|t| t.attribute(ATTR_CAPACITY).or(t.capacity) == Some(5000.0) && t.type_id != TYPE_PLASTIC_WRAP).expect("5000 m3 hold in SDE").type_id;
    let result: Result<(), CallError> = db.transaction(
        |tx, _: &mut Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                let parent = crate::dao::items::transfers::insert(tx, hold_type, 1, 1, FLAG_CARGO, 1).await.map_err(db_err)?;
                let empty = item_capacity_reply(tx, &sde, parent).await?;
                assert_eq!(empty, keyval([("capacity", Value::Float(5000.0)), ("used", Value::Float(0.0))]));
                let child = crate::dao::items::transfers::insert(tx, 587, 1, parent, FLAG_CARGO, 2).await.map_err(db_err)?;
                let packaged = item_capacity_reply(tx, &sde, parent).await?;
                assert_eq!(packaged, keyval([("capacity", Value::Float(5000.0)), ("used", Value::Float(5000.0))]));
                tx.execute("UPDATE game.items SET singleton = true, quantity = 1 WHERE item_id = $1", &[&child]).await.unwrap();
                let assembled = item_capacity_reply(tx, &sde, parent).await?;
                assert_eq!(assembled, keyval([("capacity", Value::Float(5000.0)), ("used", Value::Float(assembled_volume))]));
                let missing = crate::dao::items::transfers::insert(tx, i32::MAX, 1, parent, FLAG_CARGO, 1).await.map_err(db_err)?;
                assert!(matches!(item_capacity_reply(tx, &sde, parent).await, Err(CallError::Internal(error)) if error.to_string() == "missing SDE type 2147483647"));
                tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&missing]).await.unwrap();
                assert_eq!(item_capacity_reply(tx, &sde, i64::MAX).await?, keyval([("capacity", Value::Float(0.0)), ("used", Value::Float(0.0))]));
                Err(CallError::Internal(anyhow::anyhow!("rollback test fixtures")))
            })
        },
        |(), _: Outbox| async { Ok(()) },
    ).await;
    assert!(matches!(result, Err(CallError::Internal(error)) if error.to_string() == "rollback test fixtures"));
}
