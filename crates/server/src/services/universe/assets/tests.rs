//! Asset classification and visibility use the same production root resolver.
use super::*;
use crate::services::{self, tests::fixture, Call};
use infinity_core::consts::{FLAG_CORP_DELIVERIES, FLAG_CORP_SAG1, ROLE_ACCOUNTANT, ROLE_HANGAR_CAN_QUERY_1, TYPE_OFFICE};
use marshal::Value;

fn visible(snapshot: &mut Snapshot<'_>) -> Vec<(i64, AssetRootKind)> {
    let mut rows = Vec::new();
    snapshot.for_each_entry(|item, root| rows.push((item.item_id, root.kind)));
    rows
}

#[tokio::test]
#[ignore = "requires a dedicated test database with SDE"]
async fn roots_counts_search_and_wire_rows_share_the_same_visibility() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let picker = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
    let station_id = station.station_id;
    let system = station.solar_system_id.unwrap();
    let corp = fixture::create_corporation(&db, actor.character, station_id).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, ROLE_ACCOUNTANT).await.unwrap();
    fixture::assign_corporation(&db, picker.character, corp, ROLE_HANGAR_CAN_QUERY_1).await.unwrap();
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station_id))]);
    let ty = client.sde.types.iter().find(|t| t.published == Some(true)).unwrap().type_id;
    let office = crate::dao::items::creation::rookie_ship(&db, TYPE_OFFICE, corp, station_id, 0).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station_id, crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY, 0).await.unwrap();
    let other_station = client.sde.map.stations().find(|s| s.station_id != station_id && s.solar_system_id.is_some()).unwrap().station_id;
    let empty_office = crate::dao::items::creation::rookie_ship(&db, TYPE_OFFICE, corp, other_station, 0).await.unwrap();
    crate::dao::offices::insert(&db, empty_office, corp, other_station, crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY, 0).await.unwrap();
    let public = crate::dao::items::transfers::insert(&db, ty, corp, office, FLAG_CORP_SAG1, 3).await.unwrap();
    let hidden = crate::dao::items::transfers::insert(&db, ty, corp, office, FLAG_CORP_SAG1 + 1, 5).await.unwrap();
    let invalid = crate::dao::items::transfers::insert(&db, ty, corp, office, FLAG_HANGAR, 1).await.unwrap();
    let delivery = crate::dao::items::transfers::insert(&db, ty, corp, station_id, FLAG_CORP_DELIVERIES, 7).await.unwrap();
    let capsule = crate::dao::items::transfers::insert(&db, ty, corp, station_id, FLAG_CAPSULEER_DELIVERIES, 2).await.unwrap();
    let property = crate::dao::items::transfers::insert(&db, ty, corp, system, FLAG_HANGAR, 2).await.unwrap();
    let wrap = crate::dao::items::transfers::insert(&db, TYPE_ASSET_SAFETY_WRAP, corp, station_id, FLAG_ASSET_SAFETY, 1).await.unwrap();
    let nested_wrap = crate::dao::items::creation::rookie_ship(&db, TYPE_ASSET_SAFETY_WRAP, corp, office, FLAG_CORP_SAG1).await.unwrap();
    let wrapped_item = crate::dao::items::transfers::insert(&db, ty, corp, nested_wrap, FLAG_HANGAR, 1).await.unwrap();
    let foreign_parent = crate::dao::items::creation::rookie_ship(&db, ty, picker.character, station_id, FLAG_HANGAR).await.unwrap();
    let foreign_child = crate::dao::items::transfers::insert(&db, ty, corp, foreign_parent, FLAG_HANGAR, 1).await.unwrap();
    let cycle_a = crate::dao::items::creation::rookie_ship(&db, ty, corp, station_id, FLAG_HANGAR).await.unwrap();
    let cycle_b = crate::dao::items::creation::rookie_ship(&db, ty, corp, cycle_a, FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&cycle_a, &cycle_b]).await.unwrap();
    let mut snapshot = Snapshot::load(&client, corp).await.unwrap();
    for (id, kind) in [(public, AssetRootKind::Office), (hidden, AssetRootKind::Office), (delivery, AssetRootKind::Deliveries),
        (capsule, AssetRootKind::CapsuleerDeliveries), (property, AssetRootKind::Property), (wrap, AssetRootKind::AssetWrap)] {
        assert_eq!(snapshot.resolve(id).unwrap().kind, kind);
    }
    assert_eq!(snapshot.resolve(nested_wrap).unwrap().kind, AssetRootKind::AssetWrap);
    assert_eq!(snapshot.resolve(wrapped_item).unwrap().kind, AssetRootKind::AssetWrap);
    assert!(snapshot.resolve(foreign_child).is_none());
    assert!(snapshot.resolve(cycle_a).is_none());
    assert!(snapshot.resolve(invalid).is_none());
    let mut mismatch = Snapshot::load(&client, corp).await.unwrap();
    mismatch.nodes.get_mut(&public).unwrap().item.quantity += 1;
    assert!(!visible(&mut mismatch).iter().any(|(id, _)| *id == public));
    assert_eq!(visible(&mut snapshot).iter().filter(|(_, kind)| *kind == AssetRootKind::Office).count(), 2);
    let item = snapshot.items.iter().find(|item| item.item_id == public).unwrap();
    let exact = Filter { category: Some(item.category_id), group: Some(item.group_id), kind: Some(i64::from(item.type_id)), quantity: Some(3) };
    assert!(exact.matches(item));
    assert!(!(Filter { quantity: Some(4), ..exact }).matches(item));
    assert!(!(Filter { group: Some(item.group_id + 1), ..exact }).matches(item));
    assert_eq!(snapshot.locations(AssetRootKind::Office, None).len(), 2);
    let kwargs = Value::None;
    let args = [Value::Int(corp), Value::Int(station_id), Value::str("offices")];
    let call = Call { service: "corpmgr", method: "GetAssetInventoryForLocation", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let result = services::dispatch(&mut client, &call).await.unwrap();
    let decoded = marshal::loads(&marshal::dumps(&result)).unwrap();
    let Value::Reduce { data, .. } = decoded else { panic!("expected CRowset") };
    let parts = data.items().unwrap()[1].items().unwrap();
    let descriptor = marshal::DbRowDescriptor::from_value(&parts[0]).unwrap();
    assert!(descriptor.columns.iter().any(|(name, ty)| name == "quantity" && *ty == marshal::DbType::I8));
    assert!(descriptor.columns.iter().any(|(name, ty)| name == "categoryID" && *ty == marshal::DbType::I4));
    assert!(descriptor.virtual_columns.iter().any(|(name, _)| name == "stacksize"));
    assert!(descriptor.virtual_columns.iter().any(|(name, _)| name == "singleton"));
    let rows = parts[1].items().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|row| descriptor.get(row, "itemID") == Some(Value::Int(public))));
    let search = [Value::str("offices"), Value::None, Value::None, Value::Int(i64::from(ty)), Value::Int(4)];
    let call = Call { service: "corpmgr", method: "SearchAssets", args: &search, kwargs: &kwargs, bound: false, bind_params: None };
    let locations = services::dispatch(&mut client, &call).await.unwrap();
    assert_eq!(locations.items().unwrap().len(), 1);
    db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&public, &(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT + 1)]).await.unwrap();
    let mut capped = Snapshot::load(&client, corp).await.unwrap();
    let filter = Filter { category: None, group: None, kind: Some(i64::from(ty)), quantity: Some(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT + 1) };
    assert!(capped.locations(AssetRootKind::Office, Some(filter)).is_empty());
    let (mut restricted, _peer) = services::tests::client(db.clone(), picker.character).await;
    restricted.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station_id))]);
    let mut view = Snapshot::load(&restricted, corp).await.unwrap();
    let visible_ids: Vec<_> = visible(&mut view).into_iter().map(|(id, _)| id).collect();
    assert!(visible_ids.contains(&public));
    assert!(!visible_ids.contains(&hidden) && !visible_ids.contains(&delivery) && !visible_ids.contains(&wrap));
    db.execute("UPDATE game.offices SET lease_state=1 WHERE office_id=$1", &[&office]).await.unwrap();
    let mut blocked = Snapshot::load(&restricted, corp).await.unwrap();
    assert!(!visible(&mut blocked).iter().any(|(id, _)| *id == public));
    db.execute("UPDATE game.offices SET lease_state=0 WHERE office_id=$1", &[&office]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&picker.character]).await.unwrap();
    assert!(Snapshot::load(&restricted, corp).await.is_err());
    db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&empty_office]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&empty_office]).await.unwrap();
    db.execute("UPDATE game.offices SET lease_state=1 WHERE office_id=$1", &[&office]).await.unwrap();
    let mut inactive = Snapshot::load(&client, corp).await.unwrap();
    assert_eq!(inactive.resolve(public).unwrap().kind, AssetRootKind::Impounded);
    assert!(inactive.locations(AssetRootKind::Office, None).is_empty());
    assert_eq!(visible(&mut inactive).iter().filter(|(_, kind)| *kind == AssetRootKind::Impounded).count(), 2);
    fixture::cleanup_corporation(&db, corp, &[actor.character, picker.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, picker).await.unwrap();
}
