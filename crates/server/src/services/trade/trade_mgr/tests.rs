use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn recovery_custody_is_not_a_trade_source() {
    use crate::services::tests::fixture;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let other = fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), owner.character).await;
    let ty = client.sde.types.iter().next().unwrap().type_id;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let (item, group) = fixture::recovery_custody(&db, owner.character, station, ty).await;
    let mut trade = trades::create(&db, station, owner.character, other.character, crate::client::filetime_now()).await.unwrap();
    let trade_id = trade.id;
    let test_db = db.clone();
    let outcome = tokio::spawn(async move {
        let mut outbox = Outbox::<TradeEffect>::default();
        let result = stage(&test_db, &client.sde, &mut outbox, &mut trade, owner.character, None, &[item], station, None).await;
        assert!(matches!(result, Err(CallError::User { dict, .. }) if dict.iter().any(|(_, value)| *value == Value::unicode(messages::trade::TRADE_SOURCE_NOT_HANGAR))));
        assert!(trades::items(&test_db, trade.id).await.unwrap().is_empty());
        let row = test_db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("location_id")), (infinity_core::consts::NPC_CORP_INTERBUS, group));
    }).await;
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade_id]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
    db.execute("DELETE FROM game.repairable_recovery_groups WHERE group_id=$1", &[&group]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
    outcome.unwrap();
}
