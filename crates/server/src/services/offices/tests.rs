//! Office lifecycle regressions through the real transaction paths: rent,
//! unrent to impound, release to Deliveries, re-rent without a second fee and
//! the guarded lease deadline.

use super::*;
use crate::dao::items::loading::ItemQuery;
use crate::db::Db;
use crate::isk::IskAmount;
use crate::services::tests::fixture;
use crate::space::World;
use infinity_core::consts::{ACCOUNT_CASH, FLAG_CORP_SAG1, ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_HANGAR_CAN_TAKE_1};
use std::sync::Arc;

/// One player corporation, a director, a supported station and enough wallet
/// headroom for rent and release prices.
async fn office_fixture(db: &Db, world: &World) -> (fixture::Fixture, i64, i64) {
    let actor = fixture::create(db).await.unwrap();
    let station = world.sde().map.stations().find(|s| s.solar_system_id.is_some()).unwrap().station_id;
    let corp = fixture::create_corporation(db, actor.character, station).await.unwrap();
    fixture::assign_corporation(db, actor.character, corp, ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets (corporation_id, account_key, balance) VALUES ($1, $2, 1000000000) ON CONFLICT (corporation_id, account_key) DO UPDATE SET balance = EXCLUDED.balance", &[&corp, &ACCOUNT_CASH]).await.unwrap();
    (actor, corp, station)
}

fn identity(actor: i64, corp: i64, station: i64) -> crate::services::identity::Identity {
    crate::services::identity::Identity { char_id: actor, ship_id: None, corp_id: corp, station_id: Some(station), system_id: None }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn rent_unrent_release_and_free_rerent_roundtrip() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let (actor, corp, station) = office_fixture(&db, &world).await;
    let identity = identity(actor.character, corp, station);
    let quote = IskAmount::from_isk(rent_price(&sde, station)).unwrap();
    let office = lifecycle::rent(&identity, &db, &world, &sde, station, quote).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(station))]);
    let bind = Value::Int(station);
    let kwargs = Value::None;
    let args = [Value::Int(corp)];
    let call = Call { service: "officeManager", method: "GetPriceQuote", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    let reply = crate::services::dispatch(&mut client, &call).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), Value::Float(rent_price(&sde, station)));
    let foreign = [Value::Int(corp + 1)];
    let refused = Call { args: &foreign, ..call };
    assert!(matches!(crate::services::dispatch(&mut client, &refused).await, Err(CallError::User { .. })));
    // A second rent on an active lease refuses without debiting again.
    assert!(lifecycle::rent(&identity, &db, &world, &sde, station, quote).await.is_err());
    let ty = sde.types.iter().find(|t| t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let root = crate::dao::items::transfers::insert(&db, ty, corp, office, FLAG_CORP_SAG1, 1).await.unwrap();
    let child = crate::dao::items::transfers::insert(&db, ty, corp, root, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&vec![root, child]]).await.unwrap();
    lifecycle::unrent(&identity, &db, &world, &sde, station).await.unwrap();
    let lease = crate::dao::offices::identity(&db, corp, station).await.unwrap().unwrap();
    assert_eq!(lease.state, crate::dao::offices::LeaseState::Impounded);
    assert!(!lease.is_active(crate::client::filetime_now()));
    // An impounded lease restores for one rent fee and no release fee.
    let office2 = lifecycle::rent(&identity, &db, &world, &sde, station, quote).await.unwrap();
    assert_eq!(office2, office, "re-rent keeps the retained office identity");
    lifecycle::unrent(&identity, &db, &world, &sde, station).await.unwrap();
    let release_quote = impound::release_price(&sde, station).unwrap();
    let (notify_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    impound::release(&identity, &db, &world, &sde, notify_tx, station, release_quote).await.unwrap();
    let lease = crate::dao::offices::identity(&db, corp, station).await.unwrap().unwrap();
    assert_eq!(lease.state, crate::dao::offices::LeaseState::Released);
    for id in [root, child] {
        let item = crate::services::items::load_items(&db, &sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!((item.owner_id, item.location_id, item.flag_id),
            if id == root { (corp, station, infinity_core::consts::FLAG_CORP_DELIVERIES) } else { (corp, root, infinity_core::consts::FLAG_CARGO) });
    }
    let moved = crate::db::outbox::tests::events(&db, "ItemsMoved", actor.character).await;
    assert!(moved.iter().any(|event| matches!(event, crate::lua::events::GameEvent::ItemsMoved {
        char_id, item_id, from_location_id, to_location_id, ..
    } if *char_id == actor.character && *item_id == root && *from_location_id == office && *to_location_id == station)));
    assert!(!impound::has_impounded(&db, corp, station).await.unwrap());
    db.transaction(
        |tx, outbox: &mut crate::db::outbox::Outbox| {
            let sde = sde.clone();
            Box::pin(async move {
                let destination = crate::services::inventory::destination::Destination::owned(corp, office, FLAG_CORP_SAG1);
                crate::services::items::deposit(tx, &sde, outbox, &destination, ty, 1, 0).await?;
                Ok::<_, CallError>(())
            })
        },
        |value, outbox| {
            let world = &world;
            async move { crate::db::outbox::dispatch_world(value, outbox, world) }
        },
    ).await.unwrap();
    assert!(impound::has_impounded(&db, corp, station).await.unwrap());
    let (notify_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    impound::release(&identity, &db, &world, &sde, notify_tx, station, release_quote).await.unwrap();
    assert!(!impound::has_impounded(&db, corp, station).await.unwrap());
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn rent_refuses_a_mismatched_quote_and_insufficient_authority() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let (actor, corp, station) = office_fixture(&db, &world).await;
    let identity = identity(actor.character, corp, station);
    let wrong = IskAmount::from_isk(rent_price(&sde, station) + 1.0).unwrap();
    assert!(lifecycle::rent(&identity, &db, &world, &sde, station, wrong).await.is_err());
    db.execute("UPDATE game.characters SET corp_roles = 0, corp_roles_hq = 0 WHERE character_id = $1", &[&actor.character]).await.unwrap();
    let quote = IskAmount::from_isk(rent_price(&sde, station)).unwrap();
    assert!(lifecycle::rent(&identity, &db, &world, &sde, station, quote).await.is_err());
    let dead = sde.map.stations().map(|s| s.station_id).max().unwrap() + 1;
    let unsupported = |error: CallError| assert!(matches!(error, CallError::User { msg, dict } if msg == messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(messages::offices::OFFICE_STATION_UNSUPPORTED))]));
    unsupported(lifecycle::rent(&identity, &db, &world, &sde, dead, quote).await.err().unwrap());
    unsupported(lifecycle::unrent(&identity, &db, &world, &sde, dead).await.err().unwrap());
    unsupported(impound::release(&identity, &db, &world, &sde, tokio::sync::mpsc::unbounded_channel().0, dead, quote).await.err().unwrap());
    unsupported(impound::trash_impounded(&identity, &db, &world, &sde, tokio::sync::mpsc::unbounded_channel().0, dead).await.err().unwrap());
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(dead))]);
    unsupported(crate::services::dispatch(&mut client, &Call { service: "officeManager", method: "GetCorporationsWithOffices", args: &[], kwargs: &Value::None, bound: false, bind_params: None }).await.err().unwrap());
    client.session.apply(vec![("corpid", Value::Int(corp + 1))]);
    let refused = crate::services::dispatch(&mut client, &Call { service: "officeManager", method: "RentOffice", args: &[], kwargs: &Value::None, bound: false, bind_params: None }).await;
    assert!(matches!(&refused, Err(CallError::User { msg, dict }) if msg == messages::MSG_CUSTOM_NOTIFY && dict == &vec![("notify".into(), Value::unicode(messages::INSUFFICIENT_ROLE_FOR_OPERATION))]), "{refused:?}");
    fixture::cleanup_corporation(&db, corp, &[actor.character], 0).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn empty_office_unrents_released_and_reports_no_capacity() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let (actor, corp, station) = office_fixture(&db, &world).await;
    let identity = identity(actor.character, corp, station);
    let quote = IskAmount::from_isk(rent_price(&sde, station)).unwrap();
    let office = lifecycle::rent(&identity, &db, &world, &sde, station, quote).await.unwrap();
    lifecycle::unrent(&identity, &db, &world, &sde, station).await.unwrap();
    let lease = crate::dao::offices::identity(&db, corp, station).await.unwrap().unwrap();
    assert_eq!(lease.state, crate::dao::offices::LeaseState::Released);
    // A released office frees the slot again.
    assert_eq!(crate::dao::offices::active_count(&db, station, crate::client::filetime_now()).await.unwrap(), 0);
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn exact_deadline_and_stale_expiry_do_not_revoke_a_new_term() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let (actor, corp, station) = office_fixture(&db, &world).await;
    let office = crate::dao::items::transfers::insert(&db, TYPE_OFFICE, corp, station, 0, 1).await.unwrap();
    let boundary = crate::client::filetime_now() - 1;
    crate::dao::offices::insert(&db, office, corp, station, boundary, 0).await.unwrap();
    assert!(crate::dao::offices::active_at(&db, corp, station, boundary - 1).await.unwrap().is_some());
    assert!(crate::dao::offices::active_at(&db, corp, station, boundary).await.unwrap().is_none());
    let due = crate::dao::bills::DueLease { office_id: office, corporation_id: corp, station_id: station, rented_until: boundary };
    let next = crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY;
    assert!(crate::dao::offices::transition(&db, office, crate::dao::offices::LeaseState::Active, boundary, crate::dao::offices::LeaseState::Active, next, boundary).await.unwrap());
    let result = db.transaction(
        |tx, _: &mut ()| {
            let due = due.clone();
            Box::pin(async move { lifecycle::expire_current(tx, &due).await })
        },
        |value, ()| async move { Ok::<_, CallError>(value) },
    ).await;
    assert!(result.is_err());
    assert_eq!(crate::dao::offices::get(&db, office).await.unwrap().unwrap().rented_until, next);
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn last_available_office_slot_accepts_only_one_renter() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let (first, first_corp, station) = office_fixture(&db, &world).await;
    let second = fixture::create(&db).await.unwrap();
    let second_corp = fixture::create_corporation(&db, second.character, station).await.unwrap();
    fixture::assign_corporation(&db, second.character, second_corp, ROLE_DIRECTOR).await.unwrap();
    db.execute("INSERT INTO game.corp_wallets (corporation_id, account_key, balance) VALUES ($1,$2,1000000000) ON CONFLICT (corporation_id, account_key) DO UPDATE SET balance=EXCLUDED.balance", &[&second_corp, &ACCOUNT_CASH]).await.unwrap();
    let now = crate::client::filetime_now();
    let occupied = crate::dao::offices::active_count(&db, station, now).await.unwrap();
    let capacity = crate::config::game().corporations.office_slots_per_station;
    assert!(occupied < capacity);
    let mut fillers = Vec::new();
    for _ in 0..capacity - occupied - 1 {
        let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('slot-' || nextval('game.corporations_corporation_id_seq')::text,'S' || nextval('game.corporations_corporation_id_seq')::text,$1,$1,$2,0) RETURNING corporation_id", &[&first.character, &station]).await.unwrap().get("corporation_id");
        let office = crate::dao::items::transfers::insert(&db, TYPE_OFFICE, corp, station, 0, 1).await.unwrap();
        crate::dao::offices::insert(&db, office, corp, station, now + infinity_core::consts::FILETIME_DAY, now).await.unwrap();
        fillers.push((corp, office));
    }
    let quote = IskAmount::from_isk(rent_price(&sde, station)).unwrap();
    let one = identity(first.character, first_corp, station);
    let two = identity(second.character, second_corp, station);
    let (left, right) = tokio::join!(lifecycle::rent(&one, &db, &world, &sde, station, quote), lifecycle::rent(&two, &db, &world, &sde, station, quote));
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(crate::dao::offices::active_count(&db, station, crate::client::filetime_now()).await.unwrap(), capacity);
    for (corp, office) in fillers {
        db.execute("DELETE FROM game.offices WHERE office_id=$1", &[&office]).await.unwrap();
        db.execute("DELETE FROM game.items WHERE item_id=$1", &[&office]).await.unwrap();
        db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    }
    fixture::cleanup_corporation(&db, first_corp, &[first.character], left.unwrap_or(0)).await.unwrap();
    fixture::cleanup_corporation(&db, second_corp, &[second.character], right.unwrap_or(0)).await.unwrap();
    fixture::cleanup(&db, first).await.unwrap();
    fixture::cleanup(&db, second).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn impound_trash_offlines_a_fitted_module_inside_a_nested_hull() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let (actor, corp, station) = office_fixture(&db, &world).await;
    let identity = identity(actor.character, corp, station);
    let quote = IskAmount::from_isk(rent_price(&sde, station)).unwrap();
    let office = lifecycle::rent(&identity, &db, &world, &sde, station, quote).await.unwrap();
    let hull_type = sde.types.iter().find(|row| row.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && !sde.typelists.is_untrashable(row.type_id, row.group_id.unwrap_or_default(), infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|row| row.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
        && !sde.typelists.is_untrashable(row.type_id, row.group_id.unwrap_or_default(), infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
    let hull = crate::dao::items::creation::rookie_ship(&db, hull_type, corp, office, FLAG_CORP_SAG1).await.unwrap();
    let module = crate::dao::items::creation::rookie_ship(&db, module_type, corp, hull, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap();
    assert_eq!(crate::dao::items::combat::set_online(&db, module, hull, corp, infinity_core::consts::FLAG_FITTING_FIRST, module_type, hull_type, office, FLAG_CORP_SAG1, true, true).await.unwrap(), 1);
    lifecycle::unrent(&identity, &db, &world, &sde, station).await.unwrap();
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    impound::trash_impounded(&identity, &db, &world, &sde, sender, station).await.unwrap();
    assert!(crate::services::items::load_items(&db, &sde, ItemQuery::Ids(&[hull, module])).await.unwrap().is_empty());
    assert_eq!(crate::dao::offices::get(&db, office).await.unwrap().unwrap().state, crate::dao::offices::LeaseState::Released);
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn npc_corporation_member_offices_and_rent() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(station))]);
    let kwargs = Value::None;
    let call = Call { service: "officeManager", method: "GetMyCorporationsOffices", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let offices = crate::services::dispatch(&mut client, &call).await;
    let call = Call { service: "officeManager", method: "RentOffice", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
    let rent = crate::services::dispatch(&mut client, &call).await;
    fixture::cleanup(&db, actor).await.unwrap();
    assert_eq!(offices.unwrap(), Value::list([]));
    assert!(matches!(rent, Err(crate::services::CallError::User { msg, dict })
        if msg == messages::MSG_CUSTOM_NOTIFY
            && dict == vec![("notify".into(), Value::unicode(messages::NPC_CORPORATION_CANNOT_RENT_OFFICE))]));
}

mod client_capture;
