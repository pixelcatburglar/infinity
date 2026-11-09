use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn previous_owner_switches_do_not_survive_patch_and_root_deletion_cascades() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let a = crate::services::tests::fixture::create(&db).await.unwrap();
    let b = crate::services::tests::fixture::create(&db).await.unwrap();
    let row = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES (1,$1,$1,0,1,true) RETURNING item_id", &[&a.character]).await.unwrap();
    let ship: i64 = row.get("item_id");
    async fn write(db: &crate::db::Db, ship: i64, owner: i64, patch_value: Patch) -> bool {
        db.transaction(|tx, _: &mut crate::db::outbox::Outbox| Box::pin(async move {
            tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&ship]).await.map_err(db::Error::from)?;
            patch(tx, ship, owner, &patch_value).await
        }), |value, _| async move { Ok(value) }).await.unwrap()
    }
    assert!(write(&db, ship, a.character, Patch { fleet_hangar_fleet: Some(true), maintenance_fleet: Some(true), ..Patch::default() }).await);
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&ship, &b.character]).await.unwrap();
    assert!(!write(&db, ship, a.character, Patch::default()).await);
    assert!(write(&db, ship, b.character, Patch { maintenance_corp: Some(true), ..Patch::default() }).await);
    let result = get(&db, ship).await.unwrap().unwrap();
    assert_eq!(result.owner_id, b.character);
    assert!(!result.fleet_hangar_fleet && !result.fleet_hangar_corp && !result.maintenance_fleet && result.maintenance_corp);
    let batch = get_many(&db, &[ship, -1]).await.unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].ship_item_id, ship);
    assert_eq!(batch[0].settings.owner_id, b.character);
    assert!(batch[0].settings.maintenance_corp);
    assert!(!batch[0].settings.fleet_hangar_fleet);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    assert!(get(&db, ship).await.unwrap().is_none());
    crate::services::tests::fixture::cleanup(&db, a).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, b).await.unwrap();
}
