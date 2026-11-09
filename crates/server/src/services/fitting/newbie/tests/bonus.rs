use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn racial_bonus_is_loose_once_and_survives_reconnection() {
    let (db, sde) = database().await;
    let races: Vec<_> = sde.characters.races().filter_map(|r| r.ship_type_id.map(|ship| (r.race_id as i32, ship))).collect();
    assert_eq!(races.iter().map(|(_, ship)| *ship).collect::<std::collections::HashSet<_>>().len(), 4);
    for (race, kind) in races {
        let (f, station, system, old) = setup(&db, &sde, race, kind).await;
        let test_db = db.clone();
        let test_sde = sde.clone();
        let result = tokio::spawn(async move {
            let mut c = client(test_db.clone(), test_sde.clone(), f, station, system, old).await;
            let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
            c.notify_tx = sender.clone();
            c.world.register_pilot(f.character, sender);
            c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
            let before = snapshot(&test_db, f.character).await;
            let wallet = crate::services::wallets::balance(&test_db, f.character).await.unwrap();
            let started = crate::client::filetime_now();
            let args = [Value::Int(station)];
            let kwargs = Value::None;
            let value = rpc(&mut c, "CreateNewbieBonusItems", &args, &kwargs).await.unwrap();
            assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::None);
            let after = snapshot(&test_db, f.character).await;
            assert_eq!(after.1, Some(old));
            assert!(after.2.is_some_and(|at| at >= started && at <= crate::client::filetime_now()));
            assert_eq!(c.session.int("shipid"), Some(old));
            assert_eq!(crate::services::wallets::balance(&test_db, f.character).await.unwrap(), wallet);
            let rows = test_db.query("SELECT item_id, type_id, quantity, owner_id, location_id, flag_id, singleton, online FROM game.items WHERE owner_id=$1 AND item_id<>$2 ORDER BY item_id", &[&f.character, &old]).await.unwrap();
            assert_eq!(rows.len(), 6);
            let mut actual: Vec<_> = rows.iter().map(|r| {
                assert_eq!(r.get::<_, i64>("owner_id"), f.character);
                assert_eq!(r.get::<_, i64>("location_id"), station);
                assert_eq!(r.get::<_, i32>("flag_id"), FLAG_HANGAR);
                assert!(!r.get::<_, bool>("singleton"));
                assert!(!r.get::<_, bool>("online"));
                (r.get::<_, i32>("type_id"), r.get::<_, i64>("quantity"))
            }).collect();
            let mut expected: Vec<_> = test_sde.rookie_fits.get(kind).unwrap().items.iter().map(|&(t, _, q)| (t, i64::from(q))).collect();
            actual.sort_unstable(); expected.sort_unstable();
            assert_eq!(actual, expected);
            assert_ne!(before.0, after.0);
            let notices = item_packets(&c, false);
            assert_eq!(notices.len(), 6);
            let mut expected_rows = Vec::new();
            for row in &rows {
                let id: i64 = row.get("item_id");
                let item = crate::services::items::load_items(&test_db, &test_sde, crate::dao::items::loading::ItemQuery::Id(id)).await.unwrap().pop().unwrap();
                expected_rows.push(item.row());
            }
            assert_eq!(notices, expected_rows);
            assert!(received.try_recv().is_err());
            let queued = c.outbox.len();
            assert!(rpc(&mut c, "CreateNewbieBonusItems", &args, &kwargs).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, after);
            assert_eq!(c.outbox.len(), queued);
            assert!(received.try_recv().is_err());
            drop(c);
            drop(test_sde);
            drop(test_db);
            let db2 = Db::connect(&crate::config::tests::test_database()).await.unwrap();
            let sde2 = Arc::new(Sde::load(&db2).await.unwrap());
            let mut relog = client(db2.clone(), sde2, f, station, system, old).await;
            let (sender, mut relog_received) = tokio::sync::mpsc::unbounded_channel();
            relog.notify_tx = sender;
            assert!(rpc(&mut relog, "CreateNewbieBonusItems", &args, &kwargs).await.is_err());
            assert_eq!(snapshot(&db2, f.character).await, after);
            assert!(relog.outbox.is_empty());
            assert!(relog_received.try_recv().is_err());
        }).await;
        cleanup(&db, f).await;
        result.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn simultaneous_bonus_requests_commit_only_one_kit() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap();
    let (f, station, system, old) = setup(&db, &sde, race.race_id as i32, race.ship_type_id.unwrap()).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut first = client(test_db.clone(), sde.clone(), f, station, system, old).await;
        let mut second = client(test_db.clone(), sde, f, station, system, old).await;
        second.world = first.world.clone();
        let (first_tx, mut first_rx) = tokio::sync::mpsc::unbounded_channel();
        let (second_tx, mut second_rx) = tokio::sync::mpsc::unbounded_channel();
        first.notify_tx = first_tx;
        second.notify_tx = second_tx;
        let args = [Value::Int(station)];
        let kwargs = Value::None;
        let (a, b) = tokio::join!(rpc(&mut first, "CreateNewbieBonusItems", &args, &kwargs), rpc(&mut second, "CreateNewbieBonusItems", &args, &kwargs));
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        let (winner, loser) = if a.is_ok() { (&first, &second) } else { (&second, &first) };
        let notices = item_packets(winner, false);
        let rows = test_db.query("SELECT item_id FROM game.items WHERE owner_id=$1 AND item_id<>$2 ORDER BY item_id", &[&f.character, &old]).await.unwrap();
        let mut expected = Vec::new();
        for row in rows {
            let id: i64 = row.get("item_id");
            let item = crate::services::items::load_items(&test_db, &winner.sde, crate::dao::items::loading::ItemQuery::Id(id)).await.unwrap().pop().unwrap();
            expected.push(item.row());
        }
        assert_eq!(notices.len(), 6);
        assert_eq!(notices, expected);
        assert!(loser.outbox.is_empty());
        assert!(first_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());
        let row = test_db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1", &[&f.character]).await.unwrap();
        assert_eq!(row.get::<_, i64>("n"), 7);
        assert!(snapshot(&test_db, f.character).await.2.is_some());
    }).await;
    cleanup(&db, f).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn claiming_bonus_does_not_consume_replacement_eligibility() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, infinity_core::consts::TYPE_CAPSULE).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        assert_eq!(rpc(&mut c, "CreateNewbieBonusItems", &[Value::Int(station)], &Value::None).await.unwrap(), Value::None);
        let claimed = snapshot(&test_db, f.character).await.2;
        assert!(claimed.is_some());
        assert_eq!(rpc(&mut c, "CreateNewbieShip", &[Value::Int(old), Value::Int(station)], &Value::None).await.unwrap(), Value::None);
        assert_ne!(c.session.int("shipid"), Some(old));
        assert_eq!(snapshot(&test_db, f.character).await.2, claimed);
    }).await;
    cleanup(&db, f).await;
    result.unwrap();
}
