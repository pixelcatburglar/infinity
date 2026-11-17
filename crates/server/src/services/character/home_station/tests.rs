//! Docked `home_station.self_destruct_clone` dispatch: one committed medical
//! rebirth plus each independent validation refusal, none of which touches
//! clone custody, the wallet, insurance or combat-loss facts.
use super::*;
use crate::{dao::{characters::{clones as clone_rows, selection, ship as character_dao}, items::{brain, clones as custody, creation as item_creation, transfers}}, services::tests::{client, fixture}};
use infinity_core::{consts::{dogma::ATTR_NON_DESTRUCTIBLE, CATEGORY_IMPLANT, CATEGORY_SHIP, FLAG_IMPLANT, GROUP_BOOSTER, GROUP_CAPSULE, TYPE_CAPSULE}, messages::{self, clones::*}, session::OnlineSession};

/// Two distinct SDE stations with complete ancestry: dock source and home.
fn stations(sde: &crate::sde::Sde) -> (i64, i64) {
    let mut stations = sde.map.stations().filter(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some());
    (stations.next().unwrap().station_id, stations.next().unwrap().station_id)
}

/// An ordinary (non-capsule) ship type.
fn ordinary(sde: &crate::sde::Sde) -> i32 {
    sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.group_id != Some(GROUP_CAPSULE)).unwrap().type_id
}

/// An active implant row for the fixture owner.
async fn implant(db: &crate::db::Db, owner: i64, kind: i32, slot: i32) -> i64 {
    let item = transfers::insert(db, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=$2 WHERE item_id=$1", &[&item, &slot]).await.unwrap();
    item
}

/// A dormant jump clone holding one implant of the same kind.
async fn dormant(db: &crate::db::Db, owner: i64, location: i64, kind: i32) -> (i64, i64) {
    let clone: i64 = db.query_one("INSERT INTO game.jump_clones (owner_id,location_id) VALUES ($1,$2) RETURNING clone_id", &[&owner, &location]).await.unwrap().get("clone_id");
    let item = transfers::insert(db, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true, brain_slot=1, jump_clone_id=$2 WHERE item_id=$1", &[&item, &clone]).await.unwrap();
    (clone, item)
}

/// The unbound `home_station.self_destruct_clone` call.
async fn call(client: &mut Client, args: &[Value], kwargs: &Value, bound: bool) -> CallResult {
    crate::services::dispatch(client, &Call { service: "home_station", method: "self_destruct_clone", args, kwargs, bound, bind_params: None }).await
}

/// The refusal text of a CustomNotify result.
fn notify(result: CallResult) -> String {
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, messages::MSG_CUSTOM_NOTIFY);
            dict.into_iter().find(|(k, _)| k.as_str() == "notify")
                .and_then(|(_, v)| v.as_text().map(|t| t.into_owned())).unwrap_or_default()
        }
        _ => panic!("expected a CustomNotify refusal"),
    }
}

/// Apply the fixture client's session attributes for the boarded hull and
/// register the matching online snapshot; returns it for later reuse.
fn online(client: &mut Client, ship: i64, station: Option<i64>, system: i64) -> OnlineSession {
    let userid = client.login.user_id;
    client.session.apply(vec![
        ("userid", Value::Int(userid)),
        ("shipid", Value::Int(ship)),
        ("stationid", station.map_or(Value::None, Value::Int)),
        ("solarsystemid2", Value::Int(system)),
        ("corpid", Value::Int(1)),
    ]);
    let session = OnlineSession::from_session(&client.session).unwrap();
    client.world.register_pilot(session.character_id, client.notify_tx.clone());
    client.world.set_online_session(session.character_id, Some(session.clone()));
    session
}

/// Ship, dock, cooldown, real active-brain ids, dormant clones and killmails
/// the refusals must leave byte-identical.
async fn state(db: &crate::db::Db, owner: i64) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>, Vec<i64>, i64, i64) {
    let row = db.query_one("SELECT ship_id, station_id, solar_system_id, last_clone_jump FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    let active: Vec<i64> = brain::list(db, owner).await.unwrap().iter().map(|i| i.item_id).collect();
    let dormant: i64 = db.query_one("SELECT count(*) AS n FROM game.jump_clones WHERE owner_id=$1", &[&owner]).await.unwrap().get("n");
    let kills: i64 = db.query_one("SELECT count(*) AS n FROM game.killmails WHERE victim_id=$1", &[&owner]).await.unwrap().get("n");
    (row.get("ship_id"), row.get("station_id"), row.get("solar_system_id"), row.get("last_clone_jump"), active, dormant, kills)
}

/// Delete dormant clone custody before the fixture cleanup.
async fn drop_clones(db: &crate::db::Db, owner: i64) {
    db.execute("DELETE FROM game.items WHERE owner_id=$1 AND jump_clone_id IS NOT NULL", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id=$1", &[&owner]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn self_destruct_clone_rebirths_at_home_with_only_unprotected_losses() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (dock, home) = stations(&c.sde);
    let home_row = c.sde.map.station(home).unwrap();
    let (home_system, home_constellation, home_region) = (home_row.solar_system_id.unwrap(), home_row.constellation_id.unwrap(), home_row.region_id.unwrap());
    let dock_system = c.sde.map.station(dock).unwrap().solar_system_id.unwrap();
    let capsule = item_creation::rookie_ship(&db, TYPE_CAPSULE, f.character, dock, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3, ship_id=$4, home_station_id=$5 WHERE character_id=$1", &[&f.character, &dock, &dock_system, &capsule, &home]).await.unwrap();
    let mut kinds = c.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).map(|t| t.type_id);
    let (lost_kind, kept_kind) = (kinds.next().unwrap(), kinds.next().unwrap());
    drop(kinds);
    let lost = implant(&db, f.character, lost_kind, 1).await;
    let kept = implant(&db, f.character, kept_kind, 2).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,1)", &[&kept, &ATTR_NON_DESTRUCTIBLE]).await.unwrap();
    let (clone, dormant_item) = dormant(&db, f.character, home, kept_kind).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    c.notify_tx = tx.clone();
    let expected = online(&mut c, capsule, Some(dock), dock_system);
    assert_eq!(c.world.online_session(f.character), Some(expected));

    let reply = call(&mut c, &[], &Value::None, false).await.unwrap();
    assert_eq!(reply, Value::None);
    let replacement = selection::ship_id(&db, f.character).await.unwrap().unwrap();
    assert_ne!(replacement, capsule);
    assert!(db.query("SELECT 1 FROM game.items WHERE item_id=$1", &[&capsule]).await.unwrap().is_empty());
    // The full home session reaches the client and the registered snapshot.
    assert_eq!(c.session.int("stationid"), Some(home));
    assert!(matches!(c.session.get("solarsystemid"), Value::None));
    assert_eq!(c.session.int("solarsystemid2"), Some(home_system));
    assert_eq!(c.session.int("constellationid"), Some(home_constellation));
    assert_eq!(c.session.int("regionid"), Some(home_region));
    assert_eq!(c.session.int("shipid"), Some(replacement));
    let session = c.world.online_session(f.character).unwrap();
    assert_eq!((session.ship_id, session.station_id, session.system_id), (Some(replacement), Some(home), Some(home_system)));
    let row = db.query_one("SELECT type_id, location_id FROM game.items WHERE item_id=$1", &[&replacement]).await.unwrap();
    assert_eq!((row.get::<_, i32>("type_id"), row.get::<_, i64>("location_id")), (TYPE_CAPSULE, home));
    let location = character_dao::current(&db, f.character).await.unwrap().unwrap();
    assert_eq!((location.station, location.ship), (Some(home), Some(replacement)));
    // Only the unprotected active row died; protected and dormant rows keep
    // their item identity.
    assert!(brain::get(&db, f.character, lost).await.unwrap().is_none());
    assert!(brain::get(&db, f.character, kept).await.unwrap().is_some());
    assert_eq!(custody::list(&db, f.character, clone).await.unwrap().iter().map(|i| i.item_id).collect::<Vec<_>>(), [dormant_item]);
    // No killmail, combat-loss facts, insurance, fee or jump cooldown.
    assert!(db.query("SELECT kill_id FROM game.killmails WHERE victim_item_id=$1", &[&capsule]).await.unwrap().is_empty());
    assert!(crate::db::outbox::tests::events(&db, "LossSettled", f.character).await.is_empty());
    assert!(crate::db::outbox::tests::events(&db, "ShipLost", f.character).await.is_empty());
    assert!(db.query("SELECT 1 FROM game.insurance WHERE owner_id=$1", &[&f.character]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&f.character]).await.unwrap().is_empty());
    assert!(clone_rows::lock_character(&db, f.character).await.unwrap().unwrap().last_clone_jump.is_none());
    let notices: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok()).map(|(n, _)| n).collect();
    assert!(notices.iter().any(|n| n == "OnCloneDestruction"));
    // A repeat call is refused already-home and creates no new body.
    let before = state(&db, f.character).await;
    assert_eq!(notify(call(&mut c, &[], &Value::None, false).await), SELF_DESTRUCT_ALREADY_HOME.to_string());
    assert_eq!(state(&db, f.character).await, before);
    drop_clones(&db, f.character).await;
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn self_destruct_clone_refuses_already_home_without_mutation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (_dock, home) = stations(&c.sde);
    let home_system = c.sde.map.station(home).unwrap().solar_system_id.unwrap();
    // Boarded into an ordinary hull at home: already-home and not-capsule
    // reasons join in the client's validation order.
    let ship = item_creation::rookie_ship(&db, ordinary(&c.sde), f.character, home, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3, ship_id=$4, home_station_id=$5 WHERE character_id=$1", &[&f.character, &home, &home_system, &ship, &home]).await.unwrap();
    let kind = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).unwrap().type_id;
    implant(&db, f.character, kind, 1).await;
    let (_clone, _dormant_item) = dormant(&db, f.character, home, kind).await;
    online(&mut c, ship, Some(home), home_system);
    let before = state(&db, f.character).await;
    let reply = call(&mut c, &[], &Value::None, false).await;
    assert_eq!(notify(reply), format!("{SELF_DESTRUCT_ALREADY_HOME}\n{SELF_DESTRUCT_NOT_CAPSULE}"));
    assert_eq!(state(&db, f.character).await, before);
    drop_clones(&db, f.character).await;
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn self_destruct_clone_refuses_not_docked_without_mutation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (_dock, home) = stations(&c.sde);
    let system = c.sde.map.systems().next().unwrap().solar_system_id;
    // In space in an ordinary hull: not-docked and not-capsule join in order.
    let ship = item_creation::positioned(&db, ordinary(&c.sde), f.character, system, [0.0; 3]).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=NULL, solar_system_id=$2, ship_id=$3, home_station_id=$4 WHERE character_id=$1", &[&f.character, &system, &ship, &home]).await.unwrap();
    let kind = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).unwrap().type_id;
    implant(&db, f.character, kind, 1).await;
    online(&mut c, ship, None, system);
    let before = state(&db, f.character).await;
    let reply = call(&mut c, &[], &Value::None, false).await;
    assert_eq!(notify(reply), format!("{SELF_DESTRUCT_NOT_DOCKED}\n{SELF_DESTRUCT_NOT_CAPSULE}"));
    assert_eq!(state(&db, f.character).await, before);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn self_destruct_clone_refuses_not_capsule_without_mutation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (dock, _home) = stations(&c.sde);
    let dock_system = c.sde.map.station(dock).unwrap().solar_system_id.unwrap();
    // Docked in an ordinary hull with no explicit home: the ordinary refusal
    // precedes mandatory home resolution.
    let ship = item_creation::rookie_ship(&db, ordinary(&c.sde), f.character, dock, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3, ship_id=$4, home_station_id=NULL WHERE character_id=$1", &[&f.character, &dock, &dock_system, &ship]).await.unwrap();
    let kind = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).unwrap().type_id;
    implant(&db, f.character, kind, 1).await;
    online(&mut c, ship, Some(dock), dock_system);
    let before = state(&db, f.character).await;
    let reply = call(&mut c, &[], &Value::None, false).await;
    assert_eq!(notify(reply), SELF_DESTRUCT_NOT_CAPSULE.to_string());
    assert_eq!(state(&db, f.character).await, before);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn self_destruct_clone_rejects_bad_arguments_and_stale_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = client(db.clone(), f.character).await;
    let (dock, home) = stations(&c.sde);
    let dock_system = c.sde.map.station(dock).unwrap().solar_system_id.unwrap();
    let capsule = item_creation::rookie_ship(&db, TYPE_CAPSULE, f.character, dock, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3, ship_id=$4, home_station_id=$5 WHERE character_id=$1", &[&f.character, &dock, &dock_system, &capsule, &home]).await.unwrap();
    let kind = c.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER)).unwrap().type_id;
    implant(&db, f.character, kind, 1).await;
    let expected = online(&mut c, capsule, Some(dock), dock_system);
    let before = state(&db, f.character).await;
    for (args, kwargs, bound) in [
        (vec![Value::Int(1)], Value::None, false),
        (vec![], Value::Dict(vec![(Value::str("reason"), Value::Int(1))]), false),
        (vec![], Value::None, true),
    ] {
        let reply = call(&mut c, &args, &kwargs, bound).await;
        assert_eq!(notify(reply), SELF_DESTRUCT_INVALID_ARGUMENTS.to_string());
    }
    // A stale request may never consume a replacement hull.
    c.world.unregister_pilot_generation(f.character, Some(expected.session_id));
    assert_eq!(notify(call(&mut c, &[], &Value::None, false).await), STALE_BODY.to_string());
    // A missing explicit home stays a data fault after the live session returns.
    c.world.register_pilot(f.character, c.notify_tx.clone());
    c.world.set_online_session(f.character, Some(expected));
    db.execute("UPDATE game.characters SET home_station_id=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    assert!(matches!(call(&mut c, &[], &Value::None, false).await, Err(CallError::Internal(_))));
    assert_eq!(state(&db, f.character).await, before);
    fixture::cleanup(&db, f).await.unwrap();
}
