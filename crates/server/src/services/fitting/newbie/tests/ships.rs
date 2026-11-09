use super::*;
use infinity_core::consts::TYPE_CAPSULE;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn racial_replacement_boards_exact_fit_and_rejects_stale_request() {
    let (db, sde) = database().await;
    let races: Vec<_> = sde.characters.races().filter_map(|r| r.ship_type_id.map(|ship| (r.race_id as i32, ship))).collect();
    assert_eq!(races.iter().map(|(_, ship)| *ship).collect::<std::collections::HashSet<_>>().len(), 4);
    let rookie_group = sde.types.get(races[0].1).unwrap().group_id;
    let ordinary = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && t.group_id != rookie_group && t.type_id != TYPE_CAPSULE && t.published == Some(true)).expect("ordinary ship").type_id;
    for (race, kind) in races {
      for old_kind in [TYPE_CAPSULE, ordinary] {
        let (f, station, system, old) = setup(&db, &sde, race, old_kind).await;
        let provisioned = fixture::create(&db).await.unwrap();
        let initial = crate::services::character::provisioning::ensure_ship(&db, &sde, provisioned.character, race, station).await.unwrap();
        let provisioned_rows = db.query("SELECT type_id,flag_id,quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 ORDER BY type_id,flag_id,quantity", &[&provisioned.character, &initial]).await.unwrap();
        let provisioned_fit: Vec<_> = provisioned_rows.iter().map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("flag_id"), r.get::<_, i64>("quantity"))).collect();
        let test_db = db.clone();
        let test_sde = sde.clone();
        let result = tokio::spawn(async move {
            test_db.execute("UPDATE game.items SET name='preserved fixture' WHERE item_id=$1", &[&old]).await.unwrap();
            let child_type = test_sde.rookie_fits.get(kind).unwrap().items[0].0;
            let child = crate::dao::items::transfers::insert(&test_db, child_type, f.character, old, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
            assert!(crate::dao::items::dogma::set_override(&test_db, old, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, 1.0).await.unwrap());
            let old_row: String = test_db.query_one("SELECT row_to_json(i)::text AS value FROM game.items i WHERE item_id=$1", &[&old]).await.unwrap().get("value");
            let mut c = client(test_db.clone(), test_sde.clone(), f, station, system, old).await;
            let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
            c.notify_tx = sender.clone();
            c.world.register_pilot(f.character, sender);
            c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
            let args = [Value::Int(old), Value::Int(station)];
            let value = rpc(&mut c, "CreateNewbieShip", &args, &Value::None).await.unwrap();
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
            let new = c.session.int("shipid").unwrap();
            assert_ne!(old, new);
            assert_eq!(crate::dao::characters::selection::ship_id(&test_db, f.character).await.unwrap(), Some(new));
            let rows = test_db.query("SELECT item_id,type_id,owner_id,location_id,flag_id,quantity FROM game.items WHERE owner_id=$1 AND (item_id=$2 OR location_id=$2) ORDER BY item_id", &[&f.character, &new]).await.unwrap();
            assert_eq!(rows.len(), 7);
            let hull = rows.iter().find(|r| r.get::<_, i64>("item_id") == new).unwrap();
            assert_eq!(hull.get::<_, i32>("type_id"), kind);
            let notices = item_packets(&c, true);
            assert_eq!(notices.len(), 7);
            let mut loaded = crate::services::items::load_items(&test_db, &test_sde, crate::dao::items::loading::ItemQuery::IdOrLocationExceptFlag(new, FLAG_HANGAR)).await.unwrap();
            loaded.sort_unstable_by_key(|item| item.item_id);
            assert_eq!(notices, loaded.iter().map(|item| item.row()).collect::<Vec<_>>());
            assert_eq!(hull.get::<_, i64>("location_id"), station);
            assert_eq!(hull.get::<_, i32>("flag_id"), FLAG_HANGAR);
            let mut actual: Vec<_> = rows.iter().filter(|r| r.get::<_, i64>("item_id") != new).map(|r| (r.get::<_, i32>("type_id"), r.get::<_, i32>("flag_id"), r.get::<_, i64>("quantity"))).collect();
            let mut expected: Vec<_> = test_sde.rookie_fits.get(kind).unwrap().items.iter().map(|&(ty, flag, quantity)| (ty, flag, i64::from(quantity))).collect();
            actual.sort_unstable(); expected.sort_unstable();
            assert_eq!(actual, expected);
            assert_eq!(actual, provisioned_fit);
            assert!(crate::services::dogma::info::ship_state_core(&c, new).await.is_ok());
            let previous = test_db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&old]).await.unwrap();
            assert_eq!(previous.get::<_, i64>("owner_id"), f.character);
            assert_eq!(previous.get::<_, i64>("location_id"), station);
            let old_after: String = test_db.query_one("SELECT row_to_json(i)::text AS value FROM game.items i WHERE item_id=$1", &[&old]).await.unwrap().get("value");
            assert_eq!(old_after, old_row);
            assert_eq!(test_db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&child]).await.unwrap().get::<_, i64>("location_id"), old);
            let damage = crate::dao::items::dogma::overrides(&test_db, old).await.unwrap();
            assert_eq!(damage.iter().find(|r| r.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE).unwrap().value, 1.0);
            let after = snapshot(&test_db, f.character).await;
            assert!(after.2.is_none());
            while received.try_recv().is_ok() {}
            let queued = c.outbox.len();
            assert!(rpc(&mut c, "CreateNewbieShip", &args, &Value::None).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, after);
            assert_eq!(c.outbox.len(), queued);
            assert!(received.try_recv().is_err());
            assert_eq!(rpc(&mut c, "CreateNewbieBonusItems", &[Value::Int(station)], &Value::None).await.unwrap(), Value::None);
            assert_eq!(c.session.int("shipid"), Some(new));
        }).await;
        cleanup(&db, f).await;
        cleanup(&db, provisioned).await;
        result.unwrap();
      }
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn simultaneous_ship_requests_create_at_most_one_hull() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut first = client(test_db.clone(), sde.clone(), f, station, system, old).await;
        let mut second = client(test_db.clone(), sde, f, station, system, old).await;
        second.world = first.world.clone();
        let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
        first.notify_tx = first_tx;
        second.notify_tx = second_tx;
        let args = [Value::Int(old), Value::Int(station)];
        let kwargs = Value::None;
        let (a, b) = tokio::join!(rpc(&mut first, "CreateNewbieShip", &args, &kwargs), rpc(&mut second, "CreateNewbieShip", &args, &kwargs));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        let (winner, loser) = if a.is_ok() { (&first, &second) } else { (&second, &first) };
        let new = winner.session.int("shipid").unwrap();
        let notices = item_packets(winner, true);
        let mut loaded = crate::services::items::load_items(&test_db, &winner.sde, crate::dao::items::loading::ItemQuery::IdOrLocationExceptFlag(new, FLAG_HANGAR)).await.unwrap();
        loaded.sort_unstable_by_key(|item| item.item_id);
        assert_eq!(notices, loaded.iter().map(|item| item.row()).collect::<Vec<_>>());
        assert_eq!(notices.len(), 7);
        assert!(loser.outbox.is_empty());
        if a.is_ok() { assert!(second_rx.try_recv().is_err()); }
        else { assert!(first_rx.try_recv().is_err()); }
        assert!(crate::services::dogma::info::ship_state_core(winner, new).await.is_ok());
        let rows = test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3", &[&f.character, &station, &FLAG_HANGAR]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(snapshot(&test_db, f.character).await.1, Some(old));
    }).await;
    cleanup(&db, f).await;
    result.unwrap();
}
