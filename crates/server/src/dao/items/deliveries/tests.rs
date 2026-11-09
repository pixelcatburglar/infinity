use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL; run serially"]
async fn batch_ancestry_preserves_scalar_results() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let first = crate::dao::items::creation::rookie_ship(&db, 1, owner, owner, 0).await.unwrap();
    let second = crate::dao::items::creation::rookie_ship(&db, 1, owner, first, 0).await.unwrap();
    let third = crate::dao::items::creation::rookie_ship(&db, 1, owner, second, 0).await.unwrap();
    let rows = ancestry_many(&db, &[third, first]).await.unwrap();
    for id in [third, first] {
        let scalar = ancestry(&db, id).await.unwrap();
        let batch: Vec<_> = rows.iter().filter(|row| row.origin == id).map(|row| row.ancestor.clone()).collect();
        assert!(scalar == batch);
        assert_eq!(batch.len(), if id == third { 3 } else { 1 });
    }
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
