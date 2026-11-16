//! GM `/dirt` through the real slash dispatch: read-only status, explicit
//! foreign and live targets, forced-long wire values and appearance fences.
//! Fixtures use real items and persistent ball installation; no real client.
use super::*;
use crate::services::slash::tests::support;
use infinity_core::session::OnlineSession;
use crate::dao::items::creation;

async fn dirt(c: &mut Client, line: &str) -> Result<String, CallError> {
    support::rpc(c, line).await.map(|v| v.as_text().map(|t| t.into_owned()).unwrap_or_default())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn dirt_command_status_write_wire_and_denials() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(d.clone(), owner.character).await;
        (c.login.user_id, c.login.role) = (owner.account, infinity_core::consts::GM_ROLES);
        let station = c.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
        let (station, system) = (station.station_id, station.solar_system_id.unwrap());
        let kind = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let nonship = c.sde.types.iter().find(|t| t.category_id.is_some() && t.category_id != Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let active = creation::rookie_ship(&d, kind, owner.character, station, FLAG_HANGAR).await.unwrap();
        let untouched = creation::rookie_ship(&d, kind, owner.character, station, FLAG_HANGAR).await.unwrap();
        let object = creation::rookie_ship(&d, nonship, owner.character, station, FLAG_HANGAR).await.unwrap();
        let live = creation::positioned(&d, TYPE_CAPSULE, foreign.character, system, [0.0; 3]).await.unwrap();
        crate::dao::characters::selection::set_ship(&d, owner.character, active).await.unwrap();
        d.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&owner.character, &station, &system]).await.unwrap();
        c.session.apply(vec![("userid", Value::Int(owner.account)), ("shipid", Value::Int(active)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
        let _rx = support::register_online(&c.world, OnlineSession::from_session(&c.session).unwrap());
        // Status is read-only: a hull without a row reports uninitialized and is never written.
        assert_eq!(dirt(&mut c, "/dirt").await.unwrap(), messages::gm::gm_dirt_status(active, None));
        assert_eq!(dirt(&mut c, &format!("/dirt status {untouched}")).await.unwrap(), messages::gm::gm_dirt_status(untouched, None));
        assert_eq!(dirt_stamp(&d, untouched).await, None);
        // The explicit live foreign hull saturates to the finite 0 stamp and publishes its slim.
        admit_fixture_ship(&d, &c.world, system, foreign.character, live, false).await;
        assert!(dirt(&mut c, &format!("/dirt 1 {live}")).await.unwrap().contains(messages::gm::GM_DIRT_PUBLISHED));
        assert_eq!(dirt_stamp(&d, live).await, Some(0));
        assert_eq!(slim_stamp(&c.world.ball(system, live).await.unwrap()), Some(0));
        // A hangar write persists without a running actor; the original RPC answers a Python long.
        assert!(dirt(&mut c, &format!("/dirt 1 {active}")).await.unwrap().contains(messages::gm::GM_DIRT_NOT_LOADED));
        let bind = Value::tuple([Value::Int(station), Value::Int(infinity_core::consts::GROUP_STATION)]);
        let wire = marshal::dumps(&rpc(&mut c, "ship", "GetDirtTimestamp", &[Value::Int(active)], &bind).await.unwrap());
        assert_eq!((wire[5], marshal::loads(&wire).unwrap().as_int()), (marshal::tag::LONG, Some(0)));
        // The ordinary reset recommits the current time; cold preparation keeps the stored scalar.
        let reset = rpc(&mut c, "ship", "ResetDirtTimestamp", &[Value::Int(active)], &bind).await.unwrap().as_int().unwrap();
        assert!((crate::client::filetime_now() - reset).abs() < 60 * infinity_core::consts::FILETIME_SECOND);
        assert_eq!(rpc(&mut c, "ship", "GetDirtTimestamp", &[Value::Int(active)], &bind).await.unwrap().as_int(), Some(reset));
        assert_eq!(crate::services::ship::appearance::prepare(&d, c.sde.as_ref(), live, crate::client::filetime_now()).await.unwrap().dirt_timestamp, 0);
        let ghost = creation::positioned(&d, TYPE_CAPSULE, owner.character, system, [0.0; 3]).await.unwrap(); // after actor load: restore cannot admit it
        for line in [format!("/dirt 0.5 {object}"), format!("/dirt 0.5 {ghost}"), "/dirt 0.5 -1".into()] {
            assert!(dirt(&mut c, &line).await.is_err(), "{line}");
        }
        // An out-of-range ratio is the usage error, not a cleaning refusal.
        assert!(matches!(dirt(&mut c, "/dirt -0.1").await, Err(CallError::User { msg, dict }) if msg == messages::MSG_SLASH_ERROR && dict.iter().any(|(k, v)| k == "reason" && matches!(v, Value::Unicode(s) if s.as_str() == messages::gm::GM_DIRT_USAGE))));
        c.login.role = 0;
        assert!(dirt(&mut c, "/dirt status").await.is_err());
        for ship in [untouched, object, ghost] { assert_eq!(dirt_stamp(&d, ship).await, None, "{ship}"); }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn appearance_fence_rejects_reinstalled_and_foreign_actor() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let d = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(d.clone(), owner.character).await;
        let system = c.sde.map.stations().find_map(|s| s.solar_system_id).unwrap();
        let hull = creation::positioned(&d, TYPE_CAPSULE, owner.character, system, [0.0; 3]).await.unwrap();
        admit_fixture_ship(&d, &c.world, system, owner.character, hull, false).await;
        let fence = c.world.with(system, move |s| s.capture_appearance(hull)).await.unwrap().unwrap();
        // A same-ID reinstall is a new incarnation; the old fence publishes nothing.
        c.world.remove_ball(system, hull).await;
        admit_fixture_ship(&d, &c.world, system, owner.character, hull, false).await;
        assert_eq!(c.world.with(system, move |s| s.publish_appearance(&fence, crate::space::system::appearance::Patch { dirt_timestamp: Some(0), player_kills: None })).await, Some(false));
        // A fence captured on a different actor gate never reaches this
        // incarnation even with a coincidental generation and identical fields.
        let other_world = crate::space::World::new(d.clone(), c.sde.clone(), 1);
        admit_fixture_ship(&d, &other_world, system, owner.character, hull, false).await;
        let mut foreign_fence = other_world.with(system, move |s| s.capture_appearance(hull)).await.unwrap().unwrap();
        foreign_fence.generation = c.world.with(system, move |s| s.persistent_generation(hull)).await.unwrap().unwrap();
        assert_eq!(c.world.with(system, move |s| s.publish_appearance(&foreign_fence, crate::space::system::appearance::Patch { dirt_timestamp: Some(0), player_kills: None })).await, Some(false));
        assert_eq!(slim_stamp(&c.world.ball(system, hull).await.unwrap()), dirt_stamp(&d, hull).await);
        c.world.remove_ball(system, hull).await;
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
