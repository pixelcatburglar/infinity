//! Docked `SetShipStance` through the real dispatch chain: switch writes and
//! notices, the same-stance no-op and deadline, and refusals changing nothing.
use super::*;
use crate::services::{tests::fixture, CallError};
use infinity_core::consts::{FLAG_HANGAR, FLAG_HIDDEN_MODIFIERS, IX_LOCATION_ID};
use tokio::sync::mpsc::unbounded_channel;

fn call<'a>(args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service: "shipStanceMgr", method: "SetShipStance", args, kwargs, bound: false, bind_params: None }
}
fn refused(result: CallResult, text: &str) {
    match result { Err(CallError::User { dict, .. }) => assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]), other => panic!("expected refusal, got {other:?}") }
}
async fn docked() -> (crate::db::Db, Client, fixture::Fixture, i64, i64) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (actor, (mut c, _peer)) = (fixture::create(&db).await.unwrap(), crate::services::tests::client(db.clone(), 0).await);
    let station = c.sde.map.stations().next().unwrap().station_id;
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id",
        &[&crate::sde::ship_stances::HULL_STANCES[0].hull_type, &actor.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&actor.character, &ship]).await.unwrap();
    c.session.apply(vec![("charid", Value::Int(actor.character)), ("stationid", Value::Int(station)), ("shipid", Value::Int(ship))]);
    let bind = Value::tuple([Value::Int(ship)]);
    let init_kwargs = Value::Dict(vec![(Value::str("flag"), Value::Int(i64::from(FLAG_HIDDEN_MODIFIERS)))]);
    let initialize = Call { service: "invInventory", method: "List", args: &[], kwargs: &init_kwargs, bound: true, bind_params: Some(&bind) };
    crate::services::dispatch(&mut c, &initialize).await.unwrap();
    (db, c, actor, ship, station)
}
async fn modes(db: &crate::db::Db, ship: i64) -> Vec<(i64, i32)> {
    db.query("SELECT item_id,type_id FROM game.items WHERE location_id=$1 AND flag_id=$2 ORDER BY item_id", &[&ship, &FLAG_HIDDEN_MODIFIERS]).await.unwrap().iter().map(|r| (r.get("item_id"), r.get("type_id"))).collect()
}
async fn timing(db: &crate::db::Db, ship: i64) -> Option<(Option<i64>, i64, i64)> {
    db.query_opt("SELECT previous_stance_id,switched_at,ready_at FROM game.ship_stances WHERE ship_id=$1", &[&ship]).await.unwrap().map(|r| (r.get("previous_stance_id"), r.get("switched_at"), r.get("ready_at")))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn docked_switch_writes_item_timing_and_notices() {
    let (db, mut c, actor, ship, _) = docked().await;
    let (tx, mut rx) = unbounded_channel(); c.notify_tx = tx;
    let (args, kwargs) = ([Value::Int(ship), Value::Int(2)], Value::None);
    assert_eq!(crate::services::dispatch(&mut c, &call(&args, &kwargs)).await.unwrap(), Value::None);
    let hull = crate::sde::ship_stances::HULL_STANCES[0].hull_type;
    assert_eq!(modes(&db, ship).await.iter().map(|&(_, t)| t).collect::<Vec<_>>(), vec![crate::sde::ship_stances::mode_type(hull, 2).unwrap()]);
    let (previous, switched_at, ready_at) = timing(&db, ship).await.unwrap();
    assert!(previous == Some(1) && switched_at > 0 && ready_at > switched_at);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let changes: Vec<_> = events.iter().filter(|(event, _)| event == "OnItemChange").map(|(_, args)| args[1].clone()).collect();
    assert_eq!(changes, vec![Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(ship))]), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(0))])]);
    let active = events.iter().rposition(|(event, _)| event == "OnStanceActive").unwrap();
    assert!(active > events.iter().rposition(|(event, _)| event == "OnItemChange").unwrap());
    assert_eq!(events[active].1, vec![Value::Int(ship), Value::Int(2)]);
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn same_stance_noop_and_deadline_survive_a_fresh_client() {
    let (db, mut c, actor, ship, station) = docked().await;
    let kwargs = Value::None;
    let speed = [Value::Int(ship), Value::Int(2)];
    assert_eq!(crate::services::dispatch(&mut c, &call(&speed, &kwargs)).await.unwrap(), Value::None);
    let (tx, mut rx) = unbounded_channel(); c.notify_tx = tx;
    let before = (modes(&db, ship).await, timing(&db, ship).await);
    assert_eq!(crate::services::dispatch(&mut c, &call(&speed, &kwargs)).await.unwrap(), Value::None);
    assert_eq!((modes(&db, ship).await, timing(&db, ship).await), before);
    assert!(rx.try_recv().is_err());
    let sniper = [Value::Int(ship), Value::Int(3)];
    refused(crate::services::dispatch(&mut c, &call(&sniper, &kwargs)).await, messages::ship_stance::SWITCH_NOT_READY);
    assert_eq!((modes(&db, ship).await, timing(&db, ship).await), before);
    let (mut fresh, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    fresh.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(ship))]);
    refused(crate::services::dispatch(&mut fresh, &call(&sniper, &kwargs)).await, messages::ship_stance::SWITCH_NOT_READY);
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn refusals_leave_stance_rows_untouched() {
    let (db, mut c, actor, ship, station) = docked().await;
    let other = fixture::create(&db).await.unwrap();
    let foreign: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id",
        &[&crate::sde::ship_stances::HULL_STANCES[0].hull_type, &other.character, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    let kwargs = Value::None;
    let speed = [Value::Int(ship), Value::Int(2)];
    assert_eq!(crate::services::dispatch(&mut c, &call(&speed, &kwargs)).await.unwrap(), Value::None);
    let before = (modes(&db, ship).await, timing(&db, ship).await);
    for args in [vec![Value::Int(ship)], vec![Value::Int(ship), Value::Int(2), Value::Int(3)],
        vec![Value::Int(ship), Value::unicode("2")], vec![Value::Int(0), Value::Int(2)], vec![Value::Int(ship), Value::Int(4)]] {
        refused(crate::services::dispatch(&mut c, &call(&args, &kwargs)).await, messages::ship_stance::INVALID_REQUEST);
    }
    c.session.apply(vec![("shipid", Value::Int(foreign))]);
    refused(crate::services::dispatch(&mut c, &call(&[Value::Int(foreign), Value::Int(2)], &kwargs)).await, messages::ship_stance::INVALID_REQUEST);
    refused(crate::services::dispatch(&mut c, &call(&speed, &kwargs)).await, messages::ship_stance::INVALID_REQUEST);
    assert_eq!((modes(&db, ship).await, timing(&db, ship).await), before);
    assert!(modes(&db, foreign).await.is_empty() && timing(&db, foreign).await.is_none());
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
}
