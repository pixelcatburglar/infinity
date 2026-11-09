use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn celestial_projection_matches_source() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let projected = celestials(&db).await.unwrap();
    let map = crate::sde::map::Map::load(&db).await.unwrap();
    let source = db.query("SELECT item_id::bigint, celestial_index::bigint, radius::float8 FROM sde.map_celestials ORDER BY item_id", &[]).await.unwrap();
    assert!(!source.is_empty());
    assert_eq!(projected.len(), source.len());
    for (planet, row) in projected.iter().zip(source.iter()) {
        assert_eq!(planet.item_id, row.get::<_, i64>("item_id"));
        assert_eq!(planet.celestial_index, row.get::<_, Option<i64>>("celestial_index"));
        let loaded = map.celestial(planet.item_id).unwrap();
        assert_eq!(loaded.celestial_index, row.get::<_, Option<i64>>("celestial_index"));
        assert_eq!(loaded.radius.map(f64::to_bits), row.get::<_, Option<f64>>("radius").map(f64::to_bits));
        assert_eq!(planet.radius.map(f64::to_bits), row.get::<_, Option<f64>>("radius").map(f64::to_bits));
    }
}
