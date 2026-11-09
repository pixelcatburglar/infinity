//! Real PostgreSQL regression for hull-before-content locking during trashing.
use infinity_core::{consts::{CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR}};
use crate::{dao::items::{creation, ship, transfers}, services::tests::fixture};
use marshal::Value;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires a dedicated PostgreSQL database with SDE"]
async fn trash_locks_hull_before_older_cargo_stack() {
    const LIMIT: Duration = Duration::from_secs(120);
    const LOCK_LIMIT: Duration = Duration::from_secs(10);
    crate::tests::init_config();
    let db = tokio::time::timeout(LIMIT, crate::db::Db::connect(&crate::config::tests::test_database())).await.unwrap().unwrap();
    let actor = tokio::time::timeout(LIMIT, fixture::create(&db)).await.unwrap().unwrap();
    let test_db = db.clone();
    let mut scenario = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), actor.character).await;
        let station = client.sde.map.stations().next().expect("SDE station").station_id;
        client.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
        let trashable = |ty: &crate::sde::types::TypeData| !client.sde.typelists.is_untrashable(
            ty.type_id, ty.group_id.unwrap_or_default(), ty.category_id.unwrap_or_default());
        let hull_type = client.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP) && trashable(ty)).expect("SDE trashable hull").type_id;
        let stack_type = client.sde.types.iter().find(|ty| ty.category_id != Some(CATEGORY_SHIP) && trashable(ty)).expect("SDE trashable cargo").type_id;
        let stack = transfers::insert(&test_db, stack_type, actor.character, station, FLAG_HANGAR, 1).await.unwrap();
        let hull = creation::rookie_ship(&test_db, hull_type, actor.character, station, FLAG_HANGAR).await.unwrap();
        assert!(stack < hull, "fixture requires older stack S={stack} before hull H={hull}");
        test_db.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&stack, &hull, &FLAG_CARGO]).await.unwrap();
        tokio::time::timeout(LOCK_LIMIT, async {
        let config = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut pg, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let connection_task = tokio::spawn(async move { let _ = connection.await; });
        let tx = pg.transaction().await.unwrap();
        tx.batch_execute("SET LOCAL lock_timeout = '1s'; SET LOCAL statement_timeout = '8s'").await.unwrap();
        assert!(ship::lock_reload_ship(&tx, hull).await.unwrap());
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
        let roots = [hull];
        let service = crate::services::inventory::trashing::trash_items(&mut client, &roots, station);
        tokio::pin!(service);
        let blocked = crate::dao::items::parent_locks::tests::wait_blocked(&test_db, pid);
        tokio::pin!(blocked);
        tokio::select! {
            waiter = &mut blocked => { assert_ne!(waiter, pid); }
            result = &mut service => panic!("trash returned before blocking on hull: {result:?}"),
        }
        let (mut observer, observer_connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        let observer_task = tokio::spawn(async move { let _ = observer_connection.await; });
        let probe = observer.transaction().await.unwrap();
        probe.batch_execute("SET LOCAL lock_timeout = '1s'; SET LOCAL statement_timeout = '8s'; SAVEPOINT stack_probe").await.unwrap();
        let probe_result = probe.query_opt("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&stack]).await;
        let probe_error = probe_result.as_ref().err().map(|e| format!("SQLSTATE={} error={e}", e.code().map(|c| c.code()).unwrap_or("none")));
        let probe_code = probe_result.as_ref().err().and_then(|e| e.code()).map(|c| c.code().to_owned());
        let probe_found = probe_result.as_ref().ok().is_some_and(|row| row.is_some());
        probe.batch_execute("ROLLBACK TO SAVEPOINT stack_probe").await.unwrap();
        probe.rollback().await.unwrap();
        drop(observer);
        observer_task.abort();
        let (raw, early_service) = {
            let update = async { tx.execute("UPDATE game.items SET quantity=quantity WHERE item_id=$1", &[&stack]).await };
            tokio::pin!(update);
            tokio::select! {
                result = &mut update => (result, None),
                result = &mut service => (update.await, Some(result)),
            }
        };
        let raw_error = raw.as_ref().err().map(|e| format!("SQLSTATE={} error={e}", e.code().map(|c| c.code()).unwrap_or("none")));
        let changed = raw.as_ref().ok().copied();
        if raw.is_ok() { tx.commit().await.unwrap(); } else { tx.rollback().await.unwrap(); }
        let service_result = match early_service { Some(result) => result, None => service.await };
        drop(pg);
        connection_task.abort();
        let remaining: i64 = test_db.query_one("SELECT count(*) AS count FROM game.items WHERE item_id = ANY($1)", &[&vec![stack, hull]]).await.unwrap().get("count");
        (stack, hull, probe_error, probe_code, probe_found, raw_error, changed, service_result.is_ok(), remaining)
        }).await.expect("concurrent lock region exceeded 10 seconds")
    });
    let outcome = match tokio::time::timeout(LIMIT, &mut scenario).await {
        Ok(result) => Some(result),
        Err(_) => { scenario.abort(); let _ = scenario.await; None }
    };
    let cleanup = tokio::time::timeout(LIMIT, fixture::cleanup(&db, actor)).await;
    assert!(matches!(cleanup, Ok(Ok(()))), "fixture cleanup failed: {cleanup:?}");
    let (stack, hull, probe_error, probe_code, probe_found, raw_error, changed, service_ok, remaining) = outcome.expect("scenario exceeded 120 seconds").expect("scenario failed before cleanup");
    assert!(probe_error.is_none() && raw_error.is_none(), "older stack S={stack} was acquired before hull H={hull}; NOWAIT probe {probe_error:?}; raw stack update {raw_error:?}");
    assert!(probe_found, "NOWAIT probe did not find stack S={stack}");
    assert!(probe_code.is_none(), "unexpected NOWAIT SQLSTATE: {probe_code:?}");
    assert_eq!(changed, Some(1));
    assert!(service_ok, "trash service refused or failed");
    assert_eq!(remaining, 0, "committed trash must delete exactly the hull and its cargo stack");
}
