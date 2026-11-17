use super::*;

#[test]
fn damage_notifications_use_physical_integer_keys() {
    let id = ItemId::new(1_000_000_000_001).unwrap();
    assert_eq!(damage_dict(&BTreeMap::from([(id, 2.5)])), Value::Dict(vec![(Value::Int(id.get()), Value::Float(2.5))]));
    assert!(ItemId::new(infinity_core::consts::RECOVERY_ID_FIRST).is_none());
}
#[test]
fn loss_damage_accumulates_and_is_stable_on_both_branches() {
    let item = ItemId::new(1_000_000_000_001).unwrap();
    let seed = item.get() as u64;
    assert_eq!(15.0, loss_damage(seed, item, 5.0, 100.0, 30.0));
    assert_eq!(100.0, loss_damage(seed, item, 75.0, 100.0, 30.0));
    assert_eq!(100.0, loss_damage(seed, item, 100.0, 100.0, 30.0));
    assert_eq!(loss_damage(12, item, 5.0, 100.0, 30.0), loss_damage(12, item, 5.0, 100.0, 30.0));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn owned_global_and_single_reads_respect_authority_and_pristine_guard() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let other = fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    let type_id = client.sde.types.iter().next().unwrap().type_id;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let raw = crate::dao::items::creation::rookie_ship(&db, type_id, owner.character, station, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    let item = ItemId::new(raw).unwrap();
    assert!(service_life::set_if(&db, item, None, 2.5).await.unwrap());
    let test_db = db.clone();
    let outcome = tokio::spawn(async move {
        let identity = Identity::of_client(&client);
        assert_eq!(single(&test_db, &client.sde, &identity, item).await.unwrap(), Value::Float(2.5));
        assert_eq!(global(&test_db, &client.sde, owner.character).await.unwrap(), Value::Dict(vec![(Value::Int(raw), Value::Float(2.5))]));
        assert_eq!(global(&test_db, &client.sde, other.character).await.unwrap(), Value::Dict(vec![]));
        let foreign = Identity { char_id: other.character, ship_id: None, corp_id: 0, station_id: Some(station), system_id: None };
        assert!(single(&test_db, &client.sde, &foreign, item).await.is_err());
        assert!(require_pristine(&test_db, &[item]).await.is_err());
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
    outcome.unwrap();
}
