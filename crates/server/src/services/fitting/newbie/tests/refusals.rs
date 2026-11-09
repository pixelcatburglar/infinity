use super::*;
use infinity_core::consts::TYPE_CAPSULE;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn malformed_requests_and_location_refusals_do_not_mutate_fixture() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender;
        let before = snapshot(&test_db, f.character).await;
        let malformed = vec![
            ("CreateNewbieBonusItems", vec![], Value::None),
            ("CreateNewbieBonusItems", vec![Value::Int(0)], Value::None),
            ("CreateNewbieBonusItems", vec![Value::Bool(true)], Value::None),
            ("CreateNewbieBonusItems", vec![Value::Int(station + 1)], Value::None),
            ("CreateNewbieBonusItems", vec![Value::Int(station), Value::Int(station)], Value::None),
            ("CreateNewbieBonusItems", vec![Value::Int(station)], Value::Dict(vec![(Value::str("x"), Value::Int(1))])),
            ("CreateNewbieShip", vec![Value::Int(old)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(old + 1), Value::Int(station)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(old), Value::Int(0)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station + 1)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(0), Value::Int(station)], Value::None),
            ("CreateNewbieShip", vec![], Value::None),
            ("CreateNewbieShip", vec![Value::Bool(true), Value::Int(station)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(old), Value::Bool(true)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station), Value::Int(1)], Value::None),
            ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)], Value::Dict(vec![(Value::str("x"), Value::Int(1))])),
        ];
        for (method, args, kwargs) in malformed {
            let queued = c.outbox.len();
            assert!(rpc(&mut c, method, &args, &kwargs).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, before);
            assert_eq!(c.outbox.len(), queued);
            assert_eq!(c.session.int("shipid"), Some(old));
            assert!(received.try_recv().is_err());
        }
        for (key, value) in [("structureid", Value::Int(1)), ("solarsystemid", Value::Int(system)), ("stationid", Value::None), ("stationid", Value::Int(station + 1)), ("charid", Value::None), ("charid", Value::Int(0)), ("charid", Value::Int(-1)), ("userid", Value::None), ("userid", Value::Int(0)), ("userid", Value::Int(-1)), ("shipid", Value::None), ("shipid", Value::Int(0)), ("shipid", Value::Int(-1))] {
            let original = c.session.int(key);
            c.session.apply(vec![(key, value)]);
            for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
                assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
                assert_eq!(snapshot(&test_db, f.character).await, before);
                assert!(c.outbox.is_empty());
                assert!(received.try_recv().is_err());
            }
            c.session.apply(vec![(key, original.map(Value::Int).unwrap_or(Value::None))]);
        }
    }).await;
    cleanup(&db, f).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn authoritative_character_and_hull_refusals_leave_no_grant() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender;
        for (change, restore) in [
            ("UPDATE game.characters SET station_id=NULL WHERE character_id=$1", "UPDATE game.characters SET station_id=$2 WHERE character_id=$1"),
            ("UPDATE game.characters SET race_id=0 WHERE character_id=$1", "UPDATE game.characters SET race_id=$2 WHERE character_id=$1"),
            ("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", "UPDATE game.characters SET ship_id=$2 WHERE character_id=$1"),
        ] {
            test_db.execute(change, &[&f.character]).await.unwrap();
            let before = snapshot(&test_db, f.character).await;
            for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
                assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
                assert_eq!(snapshot(&test_db, f.character).await, before);
                assert!(c.outbox.is_empty());
                assert!(received.try_recv().is_err());
            }
            let value = if change.contains("station_id") { station } else if change.contains("race_id") { i64::from(race) } else { old };
            if change.contains("race_id") {
                test_db.execute(restore, &[&f.character, &race]).await.unwrap();
            } else {
                test_db.execute(restore, &[&f.character, &value]).await.unwrap();
            }
        }
        test_db.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&f.character]).await.unwrap();
        let before = snapshot(&test_db, f.character).await;
        for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
            assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, before);
            assert!(c.outbox.is_empty());
            assert!(received.try_recv().is_err());
        }
        test_db.execute("UPDATE game.characters SET deleted=false WHERE character_id=$1", &[&f.character]).await.unwrap();
        for (change, restore) in [
            ("UPDATE game.items SET location_id=0 WHERE item_id=$1", "UPDATE game.items SET location_id=$2 WHERE item_id=$1"),
            ("UPDATE game.items SET owner_id=0 WHERE item_id=$1", "UPDATE game.items SET owner_id=$2 WHERE item_id=$1"),
            ("UPDATE game.items SET singleton=false WHERE item_id=$1", "UPDATE game.items SET singleton=true WHERE item_id=$1"),
        ] {
            test_db.execute(change, &[&old]).await.unwrap();
            let before = snapshot(&test_db, f.character).await;
            for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
                assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
                assert_eq!(snapshot(&test_db, f.character).await, before);
                assert!(c.outbox.is_empty());
                assert!(received.try_recv().is_err());
            }
            if change.contains("singleton") { test_db.execute(restore, &[&old]).await.unwrap(); }
            else { let value = if change.contains("location_id") { station } else { f.character }; test_db.execute(restore, &[&old, &value]).await.unwrap(); }
        }
        let clone: i64 = test_db.query_one("INSERT INTO game.jump_clones (owner_id,location_id) VALUES ($1,$2) RETURNING clone_id", &[&f.character, &station]).await.unwrap().get("clone_id");
        test_db.execute("UPDATE game.items SET jump_clone_id=$2,location_id=$3,flag_id=89,brain_slot=1 WHERE item_id=$1", &[&old, &clone, &f.character]).await.unwrap();
        let before = snapshot(&test_db, f.character).await;
        for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
            assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, before);
            assert!(c.outbox.is_empty());
            assert!(received.try_recv().is_err());
        }
        test_db.execute("UPDATE game.items SET jump_clone_id=NULL,location_id=$2,flag_id=$3,brain_slot=NULL WHERE item_id=$1", &[&old, &station, &FLAG_HANGAR]).await.unwrap();
        test_db.execute("DELETE FROM game.jump_clones WHERE clone_id=$1", &[&clone]).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&f.character, &i64::MAX]).await.unwrap();
        c.session.apply(vec![("shipid", Value::Int(i64::MAX))]);
        c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
        let before = snapshot(&test_db, f.character).await;
        for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(i64::MAX), Value::Int(station)])] {
            assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, before);
            assert!(c.outbox.is_empty());
            assert!(received.try_recv().is_err());
        }
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&f.character, &old]).await.unwrap();
        assert!(c.outbox.is_empty());
    }).await;
    db.execute("UPDATE game.items SET owner_id=$2,location_id=$3,singleton=true,jump_clone_id=NULL,flag_id=$4,brain_slot=NULL WHERE item_id=$1", &[&old, &f.character, &station, &FLAG_HANGAR]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&f.character]).await.unwrap();
    cleanup(&db, f).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn superseded_session_cannot_grant_or_finish_into_replacement() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        let (caller_tx, mut caller_rx) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = caller_tx;
        let expected = infinity_core::session::OnlineSession::from_session(&c.session).unwrap();
        let mut superseding = expected.clone();
        superseding.session_id += 1;
        superseding.ship_id = Some(old + 1);
        let (superseding_tx, mut superseding_rx) = tokio::sync::mpsc::unbounded_channel();
        c.world.register_pilot(f.character, superseding_tx);
        c.world.set_online_session(f.character, Some(superseding.clone()));
        assert_eq!(c.world.online_session(f.character), Some(superseding.clone()));
        let before = snapshot(&test_db, f.character).await;
        assert!(rpc(&mut c, "CreateNewbieShip", &[Value::Int(old), Value::Int(station)], &Value::None).await.is_err());
        assert_eq!(snapshot(&test_db, f.character).await, before);
        assert_eq!(infinity_core::session::OnlineSession::from_session(&c.session), Some(expected.clone()));
        assert!(c.outbox.is_empty());
        assert!(caller_rx.try_recv().is_err());
        assert!(superseding_rx.try_recv().is_err());
        assert_eq!(c.world.online_session(f.character), Some(superseding.clone()));
        c.world.register_pilot(f.character, c.notify_tx.clone());
        c.world.set_online_session(f.character, Some(expected.clone()));
        assert_eq!(c.world.online_session(f.character), Some(expected.clone()));
        assert_eq!(rpc(&mut c, "CreateNewbieBonusItems", &[Value::Int(station)], &Value::None).await.unwrap(), Value::None);
        let marker = snapshot(&test_db, f.character).await.2;
        assert!(marker.is_some());
        c.outbox.clear();
        let _guard = crate::services::market::lock().await;
        let fences = online::capture_online_fences(&c.world, &[f.character]).await;
        let identity = Identity::of_client(&c);
        let sde = c.sde.clone();
        let now = crate::client::filetime_now();
        let new = test_db.transaction(
            move |tx, outbox: &mut Outbox<OnlineChanged>| {
                let sde = sde.clone();
                let fences = fences.clone();
                Box::pin(async move { grant_core(tx, &sde, outbox, &fences, identity, f.account, station, Grant::Ship, now).await })
            },
            |selected, outbox| {
                let c = &mut c;
                async move {
                    let world = c.world.clone();
                    online::dispatch_entries(selected, outbox, &world, |notification| match notification {
                        notification @ crate::db::outbox::Notification::DirectClient { .. } => notification.dispatch(&world, Some(&mut *c)),
                        notification => online::dispatch_notification(notification, &world, &c.notify_tx, c.session.int("charid")),
                    })
                }
            },
        ).await.unwrap().unwrap();
        let committed = snapshot(&test_db, f.character).await;
        assert_eq!(committed.1, Some(new));
        assert_eq!(committed.2, marker);
        assert_eq!(item_packets(&c, false).len(), 7);
        let mut replacement = client(test_db.clone(), c.sde.clone(), f, station, system, old).await;
        let (replacement_tx, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        replacement.notify_tx = replacement_tx;
        replacement.world = c.world.clone();
        replacement.session.sid = c.session.sid + 1;
        replacement.session.apply(vec![("shipid", Value::Int(old + 1))]);
        let replacement_session = infinity_core::session::OnlineSession::from_session(&replacement.session).unwrap();
        assert_ne!(replacement_session.session_id, expected.session_id);
        c.world.register_pilot(f.character, replacement.notify_tx.clone());
        c.world.set_online_session(f.character, Some(replacement_session.clone()));
        assert_eq!(c.world.online_session(f.character), Some(replacement_session.clone()));
        let queued = c.outbox.len();
        while caller_rx.try_recv().is_ok() {}
        assert!(crate::services::ship::boarding::finish(&mut c, Some(new), None, Some(&expected)).await.is_err());
        assert_eq!(c.outbox.len(), queued);
        assert_eq!(infinity_core::session::OnlineSession::from_session(&c.session), Some(expected.clone()));
        assert_eq!(replacement.session.int("shipid"), Some(old + 1));
        assert_eq!(infinity_core::session::OnlineSession::from_session(&replacement.session), Some(replacement_session.clone()));
        assert!(replacement.outbox.is_empty());
        assert!(caller_rx.try_recv().is_err());
        assert!(replacement_rx.try_recv().is_err());
        assert_eq!(c.world.online_session(f.character), Some(replacement_session));
        assert_eq!(snapshot(&test_db, f.character).await, committed);
    }).await;
    cleanup(&db, f).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn wrong_account_missing_character_and_current_rookie_refuse() {
    let (db, sde) = database().await;
    let racial = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap();
    let (f, station, system, old) = setup(&db, &sde, racial.race_id as i32, racial.ship_type_id.unwrap()).await;
    let other = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender;
        let before = snapshot(&test_db, f.character).await;
        assert!(rpc(&mut c, "CreateNewbieShip", &[Value::Int(old), Value::Int(station)], &Value::None).await.is_err());
        assert_eq!(snapshot(&test_db, f.character).await, before);
        for user in [other.account, i64::MAX, 0, -1] {
            c.login.user_id = user;
            c.session.apply(vec![("userid", Value::Int(user))]);
            c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
            for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
                assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
                assert_eq!(snapshot(&test_db, f.character).await, before);
                assert!(c.outbox.is_empty());
                assert!(received.try_recv().is_err());
            }
        }
        c.login.user_id = f.account;
        c.session.apply(vec![("userid", Value::Int(f.account)), ("charid", Value::Int(i64::MAX))]);
        c.world.register_pilot(i64::MAX, c.notify_tx.clone());
        c.world.set_online_session(i64::MAX, infinity_core::session::OnlineSession::from_session(&c.session));
        for (method, args) in [("CreateNewbieBonusItems", vec![Value::Int(station)]), ("CreateNewbieShip", vec![Value::Int(old), Value::Int(station)])] {
            assert!(rpc(&mut c, method, &args, &Value::None).await.is_err());
            assert_eq!(snapshot(&test_db, f.character).await, before);
            assert!(c.outbox.is_empty());
            assert!(received.try_recv().is_err());
        }
        assert!(c.outbox.is_empty());
    }).await;
    cleanup(&db, f).await;
    fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn supersession_while_market_lock_is_held_refuses_before_transaction() {
    let (db, sde) = database().await;
    let race = sde.characters.races().find(|r| r.ship_type_id.is_some()).unwrap().race_id as i32;
    let (f, station, system, old) = setup(&db, &sde, race, TYPE_CAPSULE).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let mut c = client(test_db.clone(), sde, f, station, system, old).await;
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = sender;
        let before = snapshot(&test_db, f.character).await;
        let world = c.world.clone();
        let expected = infinity_core::session::OnlineSession::from_session(&c.session).unwrap();
        let mut replacement = expected.clone();
        replacement.session_id += 1;
        let guard = crate::services::market::lock().await;
        let args = [Value::Int(station)];
        let kwargs = Value::None;
        let mut future = Box::pin(rpc(&mut c, "CreateNewbieBonusItems", &args, &kwargs));
        tokio::select! {
            biased;
            _ = &mut future => panic!("request completed while market lock was held"),
            _ = tokio::task::yield_now() => {}
        }
        let (replacement_tx, mut replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        world.register_pilot(f.character, replacement_tx);
        world.set_online_session(f.character, Some(replacement.clone()));
        assert_eq!(world.online_session(f.character), Some(replacement.clone()));
        drop(guard);
        assert!(future.await.is_err());
        assert!(c.outbox.is_empty());
        assert!(received.try_recv().is_err());
        assert!(replacement_rx.try_recv().is_err());
        assert_eq!(infinity_core::session::OnlineSession::from_session(&c.session), Some(expected));
        assert_eq!(world.online_session(f.character), Some(replacement));
        assert_eq!(snapshot(&test_db, f.character).await, before);
    }).await;
    cleanup(&db, f).await;
    result.unwrap();
}
