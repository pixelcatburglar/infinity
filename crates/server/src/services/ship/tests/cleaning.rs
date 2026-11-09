//! Dispatch tests for physical-hull dirt and explicitly deferred ship operations.
//! The client cleaning button supplies a ship ID; game items and dirt rows are fixtures.
//! These tests do not exercise the real graphical client.
use super::*;
use crate::services::tests::{client, fixture};
mod space;
mod rollback;

async fn rpc(c: &mut Client, service: &str, method: &str, args: &[Value], bind: &Value) -> CallResult {
    crate::services::dispatch(c, &Call { service, method, args, kwargs: &Value::None, bound: true, bind_params: Some(bind) }).await
}
fn refused(result: CallResult) {
    let Err(CallError::User { msg, dict }) = result else { panic!("expected user refusal") };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert!(dict.iter().any(|(k, v)| k == "notify" && matches!(v, Value::Unicode(s) if !s.trim().is_empty())));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn dirt_persists_and_station_authorization_is_guarded() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(d.clone(), owner.character).await;
        c.login.user_id = owner.account;
        let station = c.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
        let (station, system) = (station.station_id, station.solar_system_id.unwrap());
        let remote = c.sde.map.stations().find(|s| s.station_id != station).unwrap().station_id;
        let hull = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let nonship = c.sde.types.iter().find(|t| t.category_id.is_some() && t.category_id != Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let active = crate::dao::items::creation::rookie_ship(&d, hull, owner.character, station, FLAG_HANGAR).await.unwrap();
        let inactive = crate::dao::items::creation::rookie_ship(&d, hull, owner.character, station, FLAG_HANGAR).await.unwrap();
        let far = crate::dao::items::creation::rookie_ship(&d, hull, owner.character, remote, FLAG_HANGAR).await.unwrap();
        let other = crate::dao::items::creation::rookie_ship(&d, hull, foreign.character, station, FLAG_HANGAR).await.unwrap();
        let object = crate::dao::items::creation::rookie_ship(&d, nonship, owner.character, station, FLAG_HANGAR).await.unwrap();
        crate::dao::characters::selection::set_ship(&d, owner.character, active).await.unwrap();
        crate::dao::characters::ship::dock(&d, owner.character, station, system).await.unwrap();
        c.session.apply(vec![("shipid", Value::Int(active)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system)), ("solarsystemid", Value::None)]);
        let bind = Value::tuple([Value::Int(station), Value::Int(infinity_core::consts::GROUP_STATION)]);
        let args = [Value::Int(active)];
        let first = rpc(&mut c, "ship", "GetDirtTimestamp", &args, &bind).await.unwrap();
        assert!(matches!(first, Value::Int(_)));
        let reset = rpc(&mut c, "ship", "ResetDirtTimestamp", &args, &bind).await.unwrap();
        assert_eq!(rpc(&mut c, "ship", "GetDirtTimestamp", &args, &bind).await.unwrap(), reset);
        let reconnected_db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let (mut reconnect, _peer2) = client(reconnected_db, owner.character).await;
        reconnect.login.user_id = owner.account;
        reconnect.session.apply(vec![("shipid", Value::Int(active)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system)), ("solarsystemid", Value::None)]);
        assert_eq!(rpc(&mut reconnect, "ship", "GetDirtTimestamp", &args, &bind).await.unwrap(), reset);
        let inactive_args = [Value::Int(inactive)];
        let (a, b) = tokio::join!(rpc(&mut c, "ship", "GetDirtTimestamp", &inactive_args, &bind), rpc(&mut reconnect, "ship", "GetDirtTimestamp", &inactive_args, &bind));
        assert_eq!(a.unwrap(), b.unwrap());
        assert!(matches!(rpc(&mut c, "ship", "ResetDirtTimestamp", &inactive_args, &bind).await.unwrap(), Value::Int(_)));
        let module = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
        crate::dao::items::creation::rookie_ship(&d, module, owner.character, inactive, infinity_core::consts::FLAG_HI_SLOT_FIRST).await.unwrap();
        assert!(matches!(rpc(&mut c, "ship", "ResetDirtTimestamp", &inactive_args, &bind).await.unwrap(), Value::Int(_)));
        rollback::failed_reset(&mut c, &d, active, owner.character, &bind).await;
        for id in [far, other, object, i64::MAX] {
            for method in ["GetDirtTimestamp", "ResetDirtTimestamp"] { refused(rpc(&mut c, "ship", method, &[Value::Int(id)], &bind).await); }
        }
        for method in ["GetDirtTimestamp", "ResetDirtTimestamp"] {
            for args in [vec![], vec![Value::None], vec![Value::Int(-1)], vec![Value::Int(active), Value::Int(active)]] { refused(rpc(&mut c, "ship", method, &args, &bind).await); }
            let bad = Value::tuple([Value::Int(remote), Value::Int(infinity_core::consts::GROUP_STATION)]);
            refused(rpc(&mut c, "ship", method, &args, &bad).await);
        }
        c.login.user_id = foreign.account;
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &args, &bind).await);
        refused(rpc(&mut c, "ship", "GetDirtTimestamp", &args, &bind).await);
        c.login.user_id = owner.account;
        crate::dao::characters::ship::dock(&d, owner.character, remote, system).await.unwrap();
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &args, &bind).await);
        crate::dao::characters::ship::dock(&d, owner.character, station, system).await.unwrap();
        d.execute("UPDATE game.items SET singleton=false WHERE item_id=$1", &[&inactive]).await.unwrap();
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &inactive_args, &bind).await);
        d.execute("UPDATE game.items SET singleton=true, flag_id=0 WHERE item_id=$1", &[&inactive]).await.unwrap();
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &inactive_args, &bind).await);
        d.execute("UPDATE game.items SET flag_id=$2, quantity=2 WHERE item_id=$1", &[&inactive, &FLAG_HANGAR]).await.unwrap();
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &inactive_args, &bind).await);
        let clone_id: i64 = d.query_one("INSERT INTO game.jump_clones(owner_id,location_id) VALUES($1,$2) RETURNING clone_id", &[&owner.character, &station]).await.unwrap().get("clone_id");
        let clone_hull = crate::dao::items::creation::rookie_ship(&d, hull, owner.character, owner.character, infinity_core::consts::FLAG_IMPLANT).await.unwrap();
        d.execute("UPDATE game.items SET jump_clone_id=$2, brain_slot=1 WHERE item_id=$1", &[&clone_hull, &clone_id]).await.unwrap();
        for method in ["GetDirtTimestamp", "ResetDirtTimestamp"] { refused(rpc(&mut c, "ship", method, &[Value::Int(clone_hull)], &bind).await); }
        d.execute("DELETE FROM game.items WHERE item_id=$1", &[&clone_hull]).await.unwrap();
        d.execute("DELETE FROM game.jump_clones WHERE clone_id=$1", &[&clone_id]).await.unwrap();
        refused(rpc(&mut c, "ship", "ResetDirtTimestamp", &inactive_args, &bind).await);
        d.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&active, &foreign.character]).await.unwrap();
        c.login.user_id = foreign.account;
        c.session.apply(vec![("charid", Value::Int(foreign.character))]);
        assert_eq!(rpc(&mut c, "ship", "GetDirtTimestamp", &args, &bind).await.unwrap(), reset);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        for (service, method, call_args) in [
            ("ship", "LeaveShip", vec![]),
            ("ship", "SetShipPassword", vec![Value::Int(active), Value::str("fixture-password")]),
            ("dogmaIM", "AddTargetOBO", vec![Value::Int(active), Value::Int(other)]),
            ("dogmaIM", "RemoveTargetOBO", vec![Value::Int(active), Value::Int(other)]),
        ] {
            let before = c.outbox.len();
            let snapshot: Vec<_> = c.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            let fingerprint = rollback::fingerprint(&d, active).await;
            assert!(events.try_recv().is_err());
            refused(rpc(&mut c, service, method, &call_args, &bind).await);
            assert_eq!(c.outbox.len(), before, "{method}");
            assert!(events.try_recv().is_err(), "{method}");
            assert_eq!(c.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), snapshot, "{method}");
            assert_eq!(rollback::fingerprint(&d, active).await, fingerprint, "{method}");
        }
    }).await;
    db.execute(&format!("ALTER TABLE game.ship_dirt DROP CONSTRAINT IF EXISTS ship_dirt_reject_{}", owner.character), &[]).await.unwrap();
    db.execute("UPDATE game.items SET jump_clone_id=NULL WHERE owner_id=$1 AND jump_clone_id IS NOT NULL", &[&owner.character]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&owner.character]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}
