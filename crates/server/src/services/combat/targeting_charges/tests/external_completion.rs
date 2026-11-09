//! Completion of reservations from ordinary containers in a station hangar.
use super::*;
use infinity_core::consts::{FLAG_HANGAR, GROUP_CARGO_CONTAINER};
use crate::dao::items::{loading::{self, ItemQuery}, pending_reloads, transfers};

/// `const.containerHangar`, used by the bound inventory protocol.
const CONTAINER_HANGAR: i64 = 10004;

async fn rows(db: &Db, owner: i64) -> Vec<(i64, i32, i64, i64, i32, i64, bool)> {
    loading::load_items(db, ItemQuery::Owner(owner)).await.unwrap().into_iter()
        .map(|r| (r.item_id, r.type_id, r.owner_id, r.location_id, r.flag_id, r.quantity, r.singleton)).collect()
}

async fn completion_case(removed: bool) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (module_type, charge_type) = compatible_ammunition(&sde);
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let box_type = sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let fixture = crate::services::effects::tests::fixture::create(&db, ship_type, module_type, flag).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        client.session.apply(vec![("stationid", Value::Int(station))]);
        let outer = transfers::insert(&test_db, box_type, fixture.character, station, FLAG_HANGAR, 1).await.unwrap();
        let parent = transfers::insert(&test_db, box_type, fixture.character, outer, FLAG_CARGO, 1).await.unwrap();
        let source = transfers::insert(&test_db, box_type, fixture.character, parent, FLAG_CARGO, 1).await.unwrap();
        let contents = transfers::insert(&test_db, charge_type, fixture.character, source, FLAG_CARGO, 7).await.unwrap();
        test_db.execute("UPDATE game.items SET singleton=true WHERE item_id=ANY($1)", &[&&[outer, parent, source][..]]).await.unwrap();
        let id = pending_reloads::insert(&test_db, fixture.character, fixture.ship, fixture.module, flag, charge_type, 3, FLAG_CARGO, source).await.unwrap();
        let facts = pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap();
        assert_eq!(facts.len(), 1);
        let pending = PendingLoad { owner: fixture.character, reload_ids: vec![id], external_sources: vec![source], external_facts: facts, load: (fixture.module, flag, charge_type, 3, 0) };
        let snapshot = loading::capacity_snapshot(&test_db, source).await.unwrap();
        let before_used = crate::services::inventory::hold_storage::container_room(&sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap().used;
        assert_eq!(before_used, crate::services::item_volume::item_volume(&sde, charge_type, false, 10, None).unwrap());
        if removed {
            pending_reloads::take(&test_db, id).await.unwrap().unwrap();
        } else {
            let bind = Value::tuple([Value::Int(station), Value::Int(CONTAINER_HANGAR), Value::Int(fixture.character)]);
            let args = [Value::Int(source), Value::Int(parent)];
            let kwargs = Value::Dict(vec![]);
            let call = Call { service: crate::services::inventory::INVENTORY, method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
            crate::services::inventory::dispatch(&mut client, &call).await.unwrap().unwrap();
            let moved = loading::load_items(&test_db, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
            assert_eq!((moved.location_id, moved.flag_id), (station, FLAG_HANGAR));
            assert_eq!(pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap().len(), 1);
        }
        client.outbox.clear();
        let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let before = rows(&test_db, fixture.character).await;
        let completion = apply_loads(&test_db, &client.world, &notify, fixture.ship, vec![pending]).await;
        assert_eq!(rows(&test_db, fixture.character).await, before);
        let source_row = loading::load_items(&test_db, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
        let contents_row = loading::load_items(&test_db, ItemQuery::Id(contents)).await.unwrap().pop().unwrap();
        assert_eq!((source_row.location_id, contents_row.location_id, contents_row.quantity), (if removed { parent } else { station }, source, 7));
        assert!(pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap().is_empty());
        if removed {
            assert!(completion.is_err());
            assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), None);
            assert!(rx.try_recv().is_err());
            assert!(client.outbox.is_empty());
        } else {
            completion.unwrap();
            assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 3)));
            let snapshot = loading::capacity_snapshot(&test_db, source).await.unwrap();
            let after_used = crate::services::inventory::hold_storage::container_room(&sde, snapshot.parent.as_ref().unwrap(), &snapshot.items, &snapshot.reloads).unwrap().unwrap().used;
            assert_eq!(after_used, crate::services::item_volume::item_volume(&sde, charge_type, false, 7, None).unwrap());
            let (name, args) = rx.try_recv().unwrap();
            assert_eq!(name, "OnGodmaPrimeItem");
            super::charge_sync::assert_prime(&args, &sde, fixture.ship, flag, charge_type, 3);
        }
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&fixture.ship]).await.unwrap();
    crate::services::effects::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn external_completion_follows_moved_container_ancestry() { completion_case(false).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn external_completion_refuses_removed_reservation_without_writes() { completion_case(true).await; }
