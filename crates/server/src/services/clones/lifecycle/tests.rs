use super::*;
use crate::{consts::FLAG_IMPLANT, db::outbox::{Effect, Notification}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn empty_clone_destruction_is_owner_guarded_and_deletes_real_implants() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner: i64 = tx.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    let clone = create_empty_clone_tx(&tx, owner, 1).await.unwrap();
    assert!(item_dao::list(&tx, owner, clone.clone_id).await.unwrap().is_empty());
    let item: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,brain_slot,jump_clone_id) VALUES (1,$1,$1,$2,1,true,1,$3) RETURNING item_id", &[&owner, &FLAG_IMPLANT, &clone.clone_id]).await.unwrap().get("item_id");
    tx.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,1,3.5)", &[&item]).await.unwrap();
    let mut outbox = Outbox::<()>::default();
    assert!(destroy_clone_and_implants_tx(&tx, &mut outbox, owner + 1, &clone).await.is_err());
    assert_eq!(item_dao::list(&tx, owner, clone.clone_id).await.unwrap().len(), 1);
    destroy_clone_and_implants_tx(&tx, &mut outbox, owner, &clone).await.unwrap();
    assert!(tx.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&item]).await.unwrap().is_none());
    assert!(tx.query_opt("SELECT item_id FROM game.item_attributes WHERE item_id=$1", &[&item]).await.unwrap().is_none());
    let effects = outbox.into_entries();
    assert_eq!(effects.len(), 2);
    assert!(matches!(&effects[0], Effect::Notification(Notification::Pilot { event, .. }) if event == "OnJumpCloneCacheInvalidated"));
    tx.rollback().await.unwrap();
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn rename_destroy_and_reset_are_owner_guarded() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let owner = fixture.character;
    let clone = clone_dao::create(&db, owner, 1).await.unwrap();
    db.execute("UPDATE game.characters SET last_clone_jump=123 WHERE character_id=$1", &[&owner]).await.unwrap();
    let id = Identity { char_id: owner, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let foreign = Identity { char_id: owner + 1_000_000_000, ship_id: None, corp_id: 0, station_id: None, system_id: None };
    let mut outbox = Outbox::<CloneEffect>::default();
    assert!(rename_tx(&db, &mut outbox, &foreign, clone.clone_id, "foreign").await.is_err());
    assert!(rename_tx(&db, &mut outbox, &id, clone.clone_id, &"名".repeat(101)).await.is_err());
    rename_tx(&db, &mut outbox, &id, clone.clone_id, &"名".repeat(100)).await.unwrap();
    assert_eq!(clone_dao::clones(&db, owner).await.unwrap()[0].name.chars().count(), 100);
    assert!(destroy_tx(&db, &mut outbox, &foreign, clone.clone_id).await.is_err());
    reset_tx(&db, &mut outbox, &id).await.unwrap();
    assert!(clone_dao::lock_character(&db, owner).await.unwrap().unwrap().last_clone_jump.is_none());
    destroy_tx(&db, &mut outbox, &id, clone.clone_id).await.unwrap();
    assert!(clone_dao::clones(&db, owner).await.unwrap().is_empty());
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn installation_charges_once_creates_empty_body_and_rejects_duplicate() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station = client.sde.map.stations().find(|station| {
        station.station_id != 60015187
            && client.sde.industry.has_service(station.operation_id, policy::CLONING_SERVICE)
            && !client.sde.industry.has_service(station.operation_id, policy::JUMP_CLONE_FACILITY)
            && station.owner_id.is_none_or(|owner| client.sde.map.required_station_standing(owner, 24).is_none())
            && station.solar_system_id.and_then(|system| client.sde.map.system(system)).and_then(|system| system.faction_id).is_none_or(|faction| client.sde.map.required_station_standing(faction, 24).is_none())
    }).unwrap().station_id;
    let owner = fixture.character;
    let fee = policy::service_fee();
    db.execute("UPDATE game.characters SET station_id=$2,balance=$3::float8 WHERE character_id=$1", &[&owner, &station, &(fee * 2.0)]).await.unwrap();
    crate::dao::skills::training::starting_skill(&db, owner, crate::consts::SKILL_INFOMORPH_PSYCHOLOGY, 1, 250).await.unwrap();
    let id = Identity { char_id: owner, ship_id: None, corp_id: 0, station_id: Some(station), system_id: None };
    let now = crate::client::filetime_now();
    let _guard = crate::services::market::lock().await;
    db.transaction(|tx, outbox: &mut Outbox<CloneEffect>| {
        let sde = client.sde.clone();
        let id = Identity { char_id: owner, ship_id: None, corp_id: 0, station_id: Some(station), system_id: None };
        Box::pin(async move { install_tx(tx, outbox, &sde, &id, now).await })
    }, |(), _: Outbox<CloneEffect>| async move { Ok(()) }).await.unwrap();
    let rows = clone_dao::clones(&db, owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(item_dao::list(&db, owner, rows[0].clone_id).await.unwrap().is_empty());
    assert_eq!(wallets::balance(&db, owner).await.unwrap(), fee);
    let mut outbox = Outbox::default();
    assert!(install_tx(&db, &mut outbox, &client.sde, &id, now).await.is_err());
    assert_eq!(wallets::balance(&db, owner).await.unwrap(), fee);
    assert_eq!(clone_dao::destroy(&db, rows[0].clone_id, owner).await.unwrap(), 1);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn restricted_facility_checks_real_standings_and_missing_facility_is_refused() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let sde = &client.sde;
    let restricted = sde.map.stations().find_map(|station| {
        if station.station_id == 60015187 || !sde.industry.has_service(station.operation_id, policy::JUMP_CLONE_FACILITY) { return None; }
        let faction = station.solar_system_id.and_then(|system| sde.map.system(system)).and_then(|system| system.faction_id);
        let requirements: Vec<_> = [station.owner_id, faction].into_iter().flatten()
            .filter_map(|owner| sde.map.required_station_standing(owner, policy::JUMP_CLONE_FACILITY).map(|required| (owner, required))).collect();
        (!requirements.is_empty()).then_some((station.station_id, requirements))
    }).expect("station with real jump-clone standing restriction");
    let no_facility = sde.map.stations().find(|station| {
        !sde.industry.has_service(station.operation_id, policy::CLONING_SERVICE)
            && !sde.industry.has_service(station.operation_id, policy::JUMP_CLONE_FACILITY)
    }).expect("station without clone facilities").station_id;
    let owner = fixture.character;
    let now = crate::client::filetime_now();
    let fee = policy::service_fee();
    db.execute("UPDATE game.characters SET station_id=$2,balance=$3::float8 WHERE character_id=$1", &[&owner, &restricted.0, &(fee * 2.0)]).await.unwrap();
    crate::dao::skills::training::starting_skill(&db, owner, crate::consts::SKILL_INFOMORPH_PSYCHOLOGY, 1, 250).await.unwrap();
    let id = Identity { char_id: owner, ship_id: None, corp_id: 0, station_id: Some(restricted.0), system_id: None };
    let _guard = crate::services::market::lock().await;
    for &(from, required) in &restricted.1 {
        crate::dao::characters::standings::set(&db, owner, from, required - 1.0).await.unwrap();
    }
    let labels = policy::validate_install_tx(&db, sde, &id, now).await.unwrap();
    assert!(labels.contains(&Value::str(messages::clones::VALIDATION_STANDINGS)));
    let mut outbox = Outbox::default();
    assert!(install_tx(&db, &mut outbox, sde, &id, now).await.is_err());
    assert!(clone_dao::clones(&db, owner).await.unwrap().is_empty());
    for &(from, required) in &restricted.1 {
        crate::dao::characters::standings::set(&db, owner, from, required).await.unwrap();
    }
    assert!(policy::validate_install_tx(&db, sde, &id, now).await.unwrap().is_empty());
    install_tx(&db, &mut outbox, sde, &id, now).await.unwrap();
    let clones = clone_dao::clones(&db, owner).await.unwrap();
    assert_eq!(clones.len(), 1);
    assert_eq!(clone_dao::destroy(&db, clones[0].clone_id, owner).await.unwrap(), 1);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&owner, &no_facility]).await.unwrap();
    let no_bay_id = Identity { station_id: Some(no_facility), ..id };
    assert!(policy::validate_install_tx(&db, sde, &no_bay_id, now).await.unwrap().contains(&Value::str(messages::clones::VALIDATION_BAY)));
    assert!(install_tx(&db, &mut outbox, sde, &no_bay_id, now).await.is_err());
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&owner]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}