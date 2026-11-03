//! Custody transition regressions against initialized static and game data.
use super::*;
use crate::{client::filetime_now, consts::{CATEGORY_IMPLANT, GROUP_BOOSTER}, dao::items::transfers, services::tests::{client, fixture}};
mod acceptance;
/// inventorycommon.const.stationServiceJumpCloneFacility.
const JUMP_CLONE_FACILITY: i32 = 24;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn body_round_trip_retains_real_item_and_override() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = fixture::create(&db).await.unwrap();
    let (client, _peer) = client(db.clone(), fixture.character).await;
    let sde = client.sde.clone();
    let implant = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER) && t.attribute(ATTR_IMPLANTNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0) && t.attribute(ATTR_FOLLOWS_JUMP_CLONES).unwrap_or(0.0) == 0.0).expect("ordinary implant");
    let slot = implant.attribute(ATTR_IMPLANTNESS).unwrap() as i32;
    let kind = implant.type_id;
    let owner = fixture.character;
    let _market = crate::services::market::lock().await;
    let config = crate::config::tests::test_database();
    let (mut pg, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = pg.transaction().await.unwrap();
    clones::lock_character(&tx, owner).await.unwrap();
    let target = clones::create(&tx, owner, 1).await.unwrap();
    let source = clones::create(&tx, owner, 2).await.unwrap();
    let item = transfers::insert(&tx, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton=true, brain_slot=$2, brain_activated_at=$3 WHERE item_id=$1", &[&item, &slot, &42_i64]).await.unwrap();
    assert!(dogma::set_override(&tx, item, ATTR_FOLLOWS_JUMP_CLONES, 0.0).await.unwrap());
    let id = Identity { char_id: owner, ship_id: None, corp_id: 0, station_id: Some(2), system_id: None };
    let now = filetime_now();
    let mut effects = Outbox::<OnlineChanged>::default();
    let change = body_change::begin_tx(&tx, &mut effects, &sde, owner, now).await.unwrap();
    swap_body_into_source_tx(&tx, &mut effects, &sde, &id, &target, &source, now, change).await.unwrap();
    assert!(brain::get(&tx, owner, item).await.unwrap().is_none());
    let parked = custody::list(&tx, owner, source.clone_id).await.unwrap();
    assert_eq!(parked.len(), 1);
    assert_eq!((parked[0].item_id, parked[0].slot, parked[0].activated_at), (item, Some(slot), Some(42)));
    assert_eq!(custody::overrides(&tx, owner, source.clone_id, &[item]).await.unwrap()[0].value, 0.0);
    let second_source = clones::create(&tx, owner, 3).await.unwrap();
    let change = body_change::begin_tx(&tx, &mut effects, &sde, owner, now).await.unwrap();
    swap_body_into_source_tx(&tx, &mut effects, &sde, &id, &source, &second_source, now, change).await.unwrap();
    let restored = brain::get(&tx, owner, item).await.unwrap().unwrap();
    assert_eq!((restored.item_id, restored.slot, restored.activated_at), (item, Some(slot), Some(42)));
    assert_eq!(dogma::overrides(&tx, item).await.unwrap()[0].value, 0.0);
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn same_station_consumes_target_and_replay_does_not_debit() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = fixture::create(&db).await.unwrap();
    let (client, _peer) = client(db.clone(), fixture.character).await;
    let sde = client.sde.clone();
    let station = sde.map.stations().find(|station| {
        station.owner_id.is_some_and(|owner| sde.map.required_station_standing(owner, JUMP_CLONE_FACILITY).is_none())
            && station.solar_system_id.and_then(|id| sde.map.system(id)).is_some_and(|system| system.faction_id.is_none_or(|faction| sde.map.required_station_standing(faction, JUMP_CLONE_FACILITY).is_none()))
            && station.constellation_id.is_some() && station.region_id.is_some()
    }).expect("unrestricted NPC station").station_id;
    let _market = crate::services::market::lock().await;
    let config = crate::config::tests::test_database();
    let (mut pg, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = pg.transaction().await.unwrap();
    let owner = fixture.character;
    let capsule = creation::rookie_ship(&tx, TYPE_CAPSULE, owner, station, FLAG_HANGAR).await.unwrap();
    tx.execute("UPDATE game.characters SET ship_id=$2,station_id=$3,balance=$4::float8::numeric,last_clone_jump=$5 WHERE character_id=$1", &[&owner, &capsule, &station, &(policy::service_fee() * 3.0 + 1.0), &42_i64]).await.unwrap();
    let target = clones::create(&tx, owner, station).await.unwrap();
    let id = Identity { char_id: owner, ship_id: Some(capsule), corp_id: 0, station_id: Some(station), system_id: None };
    let now = filetime_now();
    let mut outbox = Outbox::default();
    assert!(jump_tx(&tx, &mut outbox, &sde, &id, station, target.clone_id, f64::NAN, false, now).await.is_err());
    assert!(jump_tx(&tx, &mut outbox, &sde, &id, station, target.clone_id + 1, policy::service_fee(), false, now).await.is_err());
    if policy::service_fee() > 0.0 {
        let save = tx.savepoint("insufficient_funds").await.unwrap();
        save.execute("UPDATE game.characters SET balance=0 WHERE character_id=$1", &[&owner]).await.unwrap();
        let mut failed_effects = Outbox::default();
        assert!(jump_tx(&save, &mut failed_effects, &sde, &id, station, target.clone_id, policy::service_fee(), false, now).await.is_err());
        assert_eq!(clones::clones(&save, owner).await.unwrap().len(), 1);
        save.rollback().await.unwrap();
    }
    let save = tx.savepoint("injected_failure").await.unwrap();
    let mut discarded = Outbox::default();
    jump_tx(&save, &mut discarded, &sde, &id, station, target.clone_id, policy::service_fee(), false, now).await.unwrap();
    assert!(save.execute("SELECT 1 / 0", &[]).await.is_err());
    save.rollback().await.unwrap();
    assert_eq!(clones::clones(&tx, owner).await.unwrap()[0].clone_id, target.clone_id);
    assert_eq!(clones::lock_character(&tx, owner).await.unwrap().unwrap().ship_id, Some(capsule));
    let before: f64 = tx.query_one("SELECT balance::float8 AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
    let result = jump_tx(&tx, &mut outbox, &sde, &id, station, target.clone_id, policy::service_fee(), false, now).await.unwrap();
    assert_ne!(result.capsule, capsule);
    let rows = clones::clones(&tx, owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].clone_id, target.clone_id);
    let state = clones::lock_character(&tx, owner).await.unwrap().unwrap();
    assert_eq!(state.last_clone_jump, Some(42));
    let balance: f64 = tx.query_one("SELECT balance::float8 AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
    assert_eq!(balance, before - policy::service_fee());
    assert!(jump_tx(&tx, &mut outbox, &sde, &id, station, target.clone_id, policy::service_fee(), true, now).await.is_err());
    let after: f64 = tx.query_one("SELECT balance::float8 AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("balance");
    assert_eq!(after, balance);
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated database with SDE"]
async fn cross_station_confirmation_and_round_trip_preserve_current_property() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = fixture::create(&db).await.unwrap();
    let (client, _peer) = client(db.clone(), fixture.character).await;
    let sde = client.sde.clone();
    let stations: Vec<_> = sde.map.stations().filter(|station| {
        station.owner_id.is_some_and(|owner| sde.map.required_station_standing(owner, JUMP_CLONE_FACILITY).is_none())
            && station.solar_system_id.and_then(|id| sde.map.system(id)).is_some_and(|system| system.faction_id.is_none_or(|faction| sde.map.required_station_standing(faction, JUMP_CLONE_FACILITY).is_none()))
            && station.constellation_id.is_some() && station.region_id.is_some()
    }).take(2).map(|station| station.station_id).collect();
    assert_eq!(stations.len(), 2);
    let (a, b) = (stations[0], stations[1]);
    let implant = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_IMPLANT) && t.group_id != Some(GROUP_BOOSTER) && t.attribute(ATTR_IMPLANTNESS).is_some_and(|v| v > 0.0 && v.fract() == 0.0) && t.attribute(ATTR_FOLLOWS_JUMP_CLONES).unwrap_or(0.0) == 0.0).unwrap();
    let kind = implant.type_id;
    let slot = implant.attribute(ATTR_IMPLANTNESS).unwrap() as i32;
    let _market = crate::services::market::lock().await;
    let config = crate::config::tests::test_database();
    let (mut pg, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = pg.transaction().await.unwrap();
    let owner = fixture.character;
    let capsule = creation::rookie_ship(&tx, TYPE_CAPSULE, owner, a, FLAG_HANGAR).await.unwrap();
    let balance = policy::service_fee() * 3.0 + 1.0;
    tx.execute("UPDATE game.characters SET ship_id=$2,station_id=$3,balance=$4::float8::numeric WHERE character_id=$1", &[&owner, &capsule, &a, &balance]).await.unwrap();
    let existing = clones::create(&tx, owner, a).await.unwrap();
    let target = clones::create(&tx, owner, b).await.unwrap();
    let lost: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot,jump_clone_id) VALUES ($1,$2,$2,$3,1,true,$4,$5) RETURNING item_id", &[&kind, &owner, &FLAG_IMPLANT, &slot, &existing.clone_id]).await.unwrap().get("item_id");
    let item = transfers::insert(&tx, kind, owner, owner, FLAG_IMPLANT, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton=true,brain_slot=$2,brain_activated_at=$3 WHERE item_id=$1", &[&item, &slot, &42_i64]).await.unwrap();
    assert!(dogma::set_override(&tx, item, ATTR_FOLLOWS_JUMP_CLONES, 0.0).await.unwrap());
    let id = Identity { char_id: owner, ship_id: Some(capsule), corp_id: 0, station_id: Some(a), system_id: None };
    let now = filetime_now();
    let mut outbox = Outbox::default();
    let error = jump_tx(&tx, &mut outbox, &sde, &id, b, target.clone_id, policy::service_fee(), false, now).await.err().unwrap();
    assert!(matches!(error, CallError::User { msg, dict } if msg == messages::clones::MSG_JUMP_CHECK_WILL_LOSE_EXISTING_CLONE && dict.is_empty()));
    assert_eq!(clones::clones(&tx, owner).await.unwrap().len(), 2);
    assert_eq!(custody::list(&tx, owner, existing.clone_id).await.unwrap()[0].item_id, lost);
    assert_eq!(tx.query_one("SELECT balance::float8 AS balance FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get::<_, f64>("balance"), balance);
    let result = jump_tx(&tx, &mut outbox, &sde, &id, b, target.clone_id, policy::service_fee(), true, now).await.unwrap();
    assert!(tx.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&lost]).await.unwrap().is_none());
    let rows = clones::clones(&tx, owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].clone_id, existing.clone_id);
    assert_eq!(rows[0].location_id, a);
    let return_target = rows[0].clone_id;
    assert_eq!(custody::list(&tx, owner, return_target).await.unwrap()[0].item_id, item);
    tx.execute("UPDATE game.characters SET last_clone_jump=NULL WHERE character_id=$1", &[&owner]).await.unwrap();
    let back_id = Identity { char_id: owner, ship_id: Some(result.capsule), corp_id: 0, station_id: Some(b), system_id: None };
    jump_tx(&tx, &mut outbox, &sde, &back_id, a, return_target, policy::service_fee(), false, now).await.unwrap();
    let restored = brain::get(&tx, owner, item).await.unwrap().unwrap();
    assert_eq!((restored.item_id, restored.slot, restored.activated_at), (item, Some(slot), Some(42)));
    assert_eq!(dogma::overrides(&tx, item).await.unwrap()[0].value, 0.0);
    tx.rollback().await.unwrap();
    fixture::cleanup(&db, fixture).await.unwrap();
}
