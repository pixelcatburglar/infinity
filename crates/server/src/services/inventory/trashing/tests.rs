use infinity_core::consts::{FLAG_HANGAR, IX_LOCATION_ID};
use crate::dao::items::loading::Item;
use crate::db::outbox::{Effect, Notification, Outbox};
use super::MoveEffect;
use super::super::notifications::notify_item_removed;

mod container_capacity;

#[test]
fn broker_maps_only_typed_trash_refusal() {
    let refusal = crate::services::user_error(infinity_core::messages::MSG_CANNOT_TRASH_ITEM, vec![]);
    assert_eq!(super::broker_refusal_key(&refusal), Some(infinity_core::messages::MSG_CANNOT_TRASH_ITEM));
    let unrelated = crate::services::user_error(infinity_core::messages::MSG_CUSTOM_NOTIFY, vec![]);
    assert_eq!(super::broker_refusal_key(&unrelated), None);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn missing_root_refuses_without_deleting_existing_items() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let (station_id, system) = {
        let station = client.sde.map.stations().next().expect("test SDE has a station");
        (station.station_id, station.solar_system_id.expect("test SDE station has a solar system"))
    };
    client.session.apply(vec![("stationid", marshal::Value::Int(station_id)), ("solarsystemid", marshal::Value::Int(system)), ("shipid", marshal::Value::Int(0))]);
    let ty = client.sde.types.iter().find(|ty| ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) && !client.sde.typelists.is_untrashable(ty.type_id, ty.group_id.unwrap_or_default(), ty.category_id.unwrap_or_default())).expect("test SDE has a trashable non-ship type");
    let item = crate::dao::items::transfers::insert(&db, ty.type_id, fixture.character, station_id, FLAG_HANGAR, 1).await.unwrap();
    let missing = item + 9_000_000_000;
    let result = super::trash_items(&mut client, &[missing], station_id).await;
    assert!(result.is_err());
    assert_eq!(db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().get::<_, i64>("n"), 1);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn broker_returns_catalogue_key_for_trash_business_refusal() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station_id = client.sde.map.stations().next().expect("test SDE has a station").station_id;
    client.session.apply(vec![("stationid", marshal::Value::Int(station_id)), ("shipid", marshal::Value::Int(0))]);
    let args = vec![
        marshal::Value::List(vec![marshal::Value::Int(9_000_000_000)]),
        marshal::Value::Int(station_id),
    ];
    let call = crate::services::Call {
        service: "invbroker",
        method: "TrashItems",
        args: &args,
        kwargs: &marshal::Value::None,
        bound: true,
        bind_params: None,
    };
    let result = crate::services::inventory::inv_broker::broker(&mut client, &call).await
        .expect("TrashItems must be handled")
        .expect("business refusal must be returned as a successful error-list result");
    assert!(matches!(result, marshal::Value::List(values)
        if values.len() == 1
            && values[0].as_text().is_some_and(|key| key == infinity_core::messages::MSG_CANNOT_TRASH_ITEM)));
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn committed_trash_removes_descendants_and_notifies_for_each_item() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station_id = client.sde.map.stations().next().expect("test SDE has a station").station_id;
    client.session.apply(vec![
        ("stationid", marshal::Value::Int(station_id)),
        ("shipid", marshal::Value::Int(0)),
    ]);
    let ty = client.sde.types.iter().find(|ty| {
        ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP)
            && !client.sde.typelists.is_untrashable(
                ty.type_id,
                ty.group_id.unwrap_or_default(),
                ty.category_id.unwrap_or_default(),
            )
    }).expect("test SDE has a trashable non-ship type");
    let parent = crate::dao::items::transfers::insert(
        &db, ty.type_id, fixture.character, station_id, FLAG_HANGAR, 1,
    ).await.unwrap();
    let child: i64 = db.query_one(
        "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
        &[&ty.type_id, &fixture.character, &parent, &FLAG_HANGAR],
    ).await.unwrap().get("item_id");
    let (notify_tx, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = notify_tx;
    let expected = crate::services::items::load_items(&db, &client.sde, crate::dao::items::loading::ItemQuery::Ids(&[parent, child])).await.unwrap();
    let expected_rows: Vec<_> = expected.into_iter().map(|item| {
        let gone = Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..item };
        gone.row()
    }).collect();
    assert_eq!(super::trash_items(&mut client, &[parent], station_id).await.unwrap(), marshal::Value::list([]));
    let received: Vec<_> = std::iter::from_fn(|| notifications.try_recv().ok()).collect();
    assert_eq!(received.len(), 2, "one OnItemChange per removed item");
    for row in expected_rows {
        assert!(received.iter().any(|(event, args)| event == "OnItemChange" && args.first() == Some(&row)));
    }
    assert_eq!(
        db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=ANY($1)", &[&vec![parent, child]]).await.unwrap().get::<_, i64>("n"),
        0,
    );
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database"]
async fn refused_trash_rolls_back_parent_and_descendants_without_notifications() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station_id = client.sde.map.stations().next().expect("test SDE has a station").station_id;
    client.session.apply(vec![
        ("stationid", marshal::Value::Int(station_id)),
        ("shipid", marshal::Value::Int(0)),
    ]);
    let ty = client.sde.types.iter().find(|ty| {
        ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP)
            && !client.sde.typelists.is_untrashable(
                ty.type_id,
                ty.group_id.unwrap_or_default(),
                ty.category_id.unwrap_or_default(),
            )
    }).expect("test SDE has a trashable non-ship type");
    let parent = crate::dao::items::transfers::insert(
        &db, ty.type_id, fixture.character, station_id, FLAG_HANGAR, 1,
    ).await.unwrap();
    let child: i64 = db.query_one(
        "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
        &[&ty.type_id, &fixture.character, &parent, &FLAG_HANGAR],
    ).await.unwrap().get("item_id");
    let (notify_tx, mut notifications) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = notify_tx;

    assert!(super::trash_items(&mut client, &[parent, parent + 9_000_000_000], station_id).await.is_err());
    assert_eq!(
        db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=ANY($1)", &[&vec![parent, child]]).await.unwrap().get::<_, i64>("n"),
        2,
    );
    assert!(notifications.try_recv().is_err());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}


#[test]
fn committed_trash_queues_old_location_removal_notification() {
    let item = Item {
        item_id: 42,
        type_id: 7,
        owner_id: 9,
        location_id: 60003760,
        flag_id: FLAG_HANGAR,
        quantity: 1,
        singleton: true,
        group_id: 1,
        category_id: 6,
        custom_info: None,
    };
    let mut outbox: Outbox<MoveEffect> = Outbox::default();
    notify_item_removed(&mut outbox, item);
    let entries = outbox.into_entries();
    assert_eq!(entries.len(), 1);
    let Effect::Notification(Notification::DirectClient { event, args }) = &entries[0] else {
        panic!("trash must queue a direct removal notification")
    };
    assert_eq!(event, "OnItemChange");
    assert!(matches!(&args[1], marshal::Value::Dict(fields) if fields.iter().any(|(key, value)| key.as_int() == Some(IX_LOCATION_ID) && value.as_int() == Some(60003760))));
}
