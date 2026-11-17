use super::{expiry_time, FILETIME_MINUTE};

mod requested_acceptance;
mod corporate;
mod corporate_auction;
mod corporate_fixture;
mod corporate_edges;
mod requested_active;
mod transitions;
mod fees;
mod volume;
mod wrap;
mod gm_expiry;
mod mixed_ownership;


#[test]
fn expiry_time_preserves_minutes_and_saturates_large_durations() {
    assert_eq!(expiry_time(42, 2), 42 + 2 * FILETIME_MINUTE);
    assert_eq!(expiry_time(42, i128::from(i64::MAX)), i64::MAX);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn recovery_custody_is_not_a_contract_source() {
    use crate::services::tests::fixture;
    use marshal::Value;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    let ty = client.sde.types.iter().next().unwrap().type_id;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let (item, group) = fixture::recovery_custody(&db, owner.character, station, ty).await;
    let test_db = db.clone();
    let outcome = tokio::spawn(async move {
        let before: i64 = test_db.query_one("SELECT count(*) AS n FROM game.contracts WHERE issuer_id=$1", &[&owner.character]).await.unwrap().get("n");
        let info = Value::dict([
            ("contractType", Value::Int(i64::from(super::TYPE_ITEM_EXCHANGE))),
            ("startStationID", Value::Int(station)),
            ("itemList", Value::list([Value::tuple([Value::Int(item), Value::Int(1)])])),
        ]);
        let result = super::create(&client, &info).await;
        assert!(matches!(result, Err(crate::services::CallError::User { dict, .. }) if dict.iter().any(|(_, value)| *value == Value::unicode(infinity_core::messages::CONTRACT_ITEM_MUST_BE_IN_START_HANGAR))));
        let after: i64 = test_db.query_one("SELECT count(*) AS n FROM game.contracts WHERE issuer_id=$1", &[&owner.character]).await.unwrap().get("n");
        assert_eq!(before, after);
        let row = test_db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id")), (infinity_core::consts::NPC_CORP_INTERBUS, group));
    }).await;
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    db.execute("DELETE FROM game.repairable_recovery_groups WHERE group_id=$1", &[&group]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    outcome.unwrap();
}
