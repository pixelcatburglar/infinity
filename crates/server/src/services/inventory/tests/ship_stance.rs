//! The owner's hidden-mode list shows the real singleton row; standalone move
//! or trash of the internal modifier item is refused without changes.
use super::*;
use infinity_core::consts::FLAG_HIDDEN_MODIFIERS;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn owner_lists_mode_item_but_standalone_mutation_is_refused() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let stances = &crate::sde::ship_stances::HULL_STANCES[0];
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap().station_id;
    client.session.apply(vec![("stationid", Value::Int(station))]);
    let ship = crate::dao::items::transfers::insert(&db, stances.hull_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
    let bind = Value::tuple([Value::Int(ship)]);
    let kwargs = Value::Dict(vec![(Value::str("flag"), Value::Int(i64::from(FLAG_HIDDEN_MODIFIERS)))]);
    let call = Call { service: INVENTORY, method: "List", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    // Listing the hidden flag twice still produces exactly one real singleton row.
    for _ in 0..2 {
        assert!(matches!(crate::services::dispatch(&mut client, &call).await.unwrap(), Value::List(rows) if rows.len() == 1));
    }
    let mode = db.query_one("SELECT item_id,type_id,singleton,quantity FROM game.items WHERE location_id=$1 AND flag_id=$2",
        &[&ship, &FLAG_HIDDEN_MODIFIERS]).await.unwrap();
    assert_eq!((mode.get::<_, i32>("type_id"), mode.get::<_, bool>("singleton"), mode.get::<_, i64>("quantity")), (stances.modes[0].1, true, 1));
    let mode_id: i64 = mode.get("item_id");
    // Standalone move or trash of the mode item is refused; the row survives.
    let moved = move_item(&crate::services::identity::Identity::of_client(&client), &db, &client.world.clone(), &client.sde.clone(), None,
        client.notify_tx.clone(), &Container::Hangar { station_id: station, owner_id: fixture.character },
        mode_id, None, Some(FLAG_HANGAR), None, |_| Ok(())).await;
    assert!(matches!(moved, Err(CallError::User { .. })));
    let trashed = crate::services::inventory::trashing::trash_items(&mut client, &[mode_id], station).await;
    assert!(matches!(trashed, Err(CallError::User { msg, .. }) if msg == messages::MSG_CANNOT_TRASH_ITEM));
    assert!(db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&mode_id]).await.unwrap().is_some());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
