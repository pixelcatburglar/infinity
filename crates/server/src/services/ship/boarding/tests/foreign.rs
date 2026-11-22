//! Foreign in-space Board: one real transfer and the refusals that roll it back.
use super::*;
use crate::dao::items::{creation, space_ships::FreeShip};
use crate::services::ship::balls;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_takes_nested_owners_and_voids_changed_insurance() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable type").type_id;
    let module = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && t.published == Some(true)
        && ATTR_REQUIRED_SKILLS.iter().any(|attr| crate::services::fit::base_attribute(Some(t), sde.dogma.attribute(*attr), *attr) > 0.0)).expect("skilled module").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let npc_corp = sde.npc_corporations.iter().next().expect("SDE NPC corporation").corporation_id;
    db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&owner, &npc_corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let container = creation::rookie_ship(&test_db, charge, foreign_owner, target, infinity_core::consts::FLAG_CARGO).await.unwrap();
        let nested = creation::rookie_ship(&test_db, charge, owner, container, infinity_core::consts::FLAG_CARGO).await.unwrap();
        let deepest = creation::rookie_ship(&test_db, charge, foreign_owner, nested, infinity_core::consts::FLAG_CARGO).await.unwrap();
        let fitted = creation::rookie_ship(&test_db, module, foreign_owner, target, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap();
        test_db.execute("UPDATE game.items SET online=true WHERE item_id=$1", &[&fitted]).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        crate::dao::insurance::insure(&test_db, target, foreign_owner, kind, 0.5, 1, i64::MAX).await.unwrap();
        crate::dao::insurance::insure(&test_db, old, owner, kind, 0.5, 1, i64::MAX).await.unwrap();
        let clone = crate::dao::characters::clones::create(&test_db, foreign_owner, 1).await.unwrap();
        let dormant = creation::rookie_ship(&test_db, 1, foreign_owner, foreign_owner, infinity_core::consts::FLAG_IMPLANT).await.unwrap();
        crate::dao::characters::clones::lock_character(&test_db, foreign_owner).await.unwrap().unwrap();
        assert!(crate::dao::items::brain::install(&test_db, dormant, foreign_owner, foreign_owner, infinity_core::consts::FLAG_IMPLANT, infinity_core::consts::FLAG_IMPLANT, 1, None, None, None).await.unwrap());
        assert!(crate::dao::items::clones::park(&test_db, foreign_owner, clone.clone_id, dormant).await.unwrap());
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let pilot = balls::piloted(&test_db, sde.clone(), &FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: npc_corp, alliance_id: -1, pos }, owner, 0.0).await.unwrap();
        let parked = balls::unpiloted(&test_db, sde.clone(), &FreeShip { item_id: target, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| { pilot.install(s, |s, ball| s.add_ship(ball)); parked.install(s, |s, ball| s.add_persistent_ship(ball)); }).await.unwrap();
        let mut c = client(test_db.clone(), sde, world.clone(), account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&world, owner, c.notify_tx.clone());
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        let before = crate::services::crimewatch::control::set_timer(&test_db, &world, owner, crate::services::crimewatch::control::TimerChange { kind: crate::services::crimewatch::control::TimerKind::Suspect, duration_ms: 60_000 }).await.unwrap();
        fourtuple(super::super::board(&mut c, target, Some(old)).await.ok().expect("foreign Board"));
        let after = crate::services::crimewatch::control::snapshot(&test_db, owner).await.unwrap();
        assert_eq!(after.suspect, before.suspect);
        assert!(crate::dao::insurance::contract(&test_db, target).await.unwrap().is_none());
        assert!(crate::dao::insurance::contract(&test_db, old).await.unwrap().is_some());
        let owners: Vec<(i64, i64)> = test_db.query("SELECT item_id, owner_id FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![target, container, nested, deepest, fitted]]).await.unwrap().iter().map(|row| (row.get("item_id"), row.get("owner_id"))).collect();
        assert_eq!(owners, [(target, owner), (container, owner), (nested, owner), (deepest, owner), (fitted, owner)]);
        assert!(test_db.query("SELECT item_id FROM game.items WHERE item_id=$1 AND online", &[&fitted]).await.unwrap().is_empty());
        let ball = world.ball(system, target).await.unwrap();
        assert_eq!((ball.owner_id, ball.corp_id, ball.alliance_id, ball.char_id), (owner, npc_corp, -1, Some(owner)));
        assert_eq!(world.ball(system, old).await.unwrap().owner_id, owner);
        let parked = test_db.query_one("SELECT owner_id, jump_clone_id FROM game.items WHERE item_id=$1", &[&dormant]).await.unwrap();
        assert_eq!(parked.get::<_, i64>("owner_id"), foreign_owner);
        assert!(parked.get::<_, Option<i64>>("jump_clone_id").is_some());
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_refuses_protected_occupied_and_station_targets() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station_row = sde.map.stations().find(|row| row.solar_system_id.is_some()).expect("SDE station");
    let (station, system) = (station_row.station_id, station_row.solar_system_id.expect("station system"));
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let protected = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let occupied = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let capsule = crate::dao::items::transfers::insert(&test_db, infinity_core::consts::TYPE_CAPSULE, foreign_owner, protected, infinity_core::consts::FLAG_CARGO, 1).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let pilot = balls::piloted(&test_db, sde.clone(), &FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos }, owner, 0.0).await.unwrap();
        let protected_ship = balls::unpiloted(&test_db, sde.clone(), &FreeShip { item_id: protected, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
        let occupied_ship = balls::unpiloted(&test_db, sde.clone(), &FreeShip { item_id: occupied, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| {
            pilot.install(s, |s, ball| s.add_ship(ball));
            protected_ship.install(s, |s, ball| s.add_persistent_ship(ball));
            occupied_ship.install(s, |s, ball| s.add_persistent_ship(ball));
        }).await.unwrap();
        let mut c = client(test_db.clone(), sde, world.clone(), account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&world, owner, c.notify_tx.clone());
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        test_db.execute("UPDATE game.characters SET ship_id=$2, deleted=true WHERE character_id=$1", &[&foreign_owner, &occupied]).await.unwrap();
        assert!(super::super::board(&mut c, protected, Some(old)).await.is_err());
        assert!(super::super::board(&mut c, occupied, Some(old)).await.is_err());
        assert_eq!(test_db.query_one("SELECT owner_id FROM game.items WHERE item_id=$1", &[&capsule]).await.unwrap().get::<_, i64>("owner_id"), foreign_owner);
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(old));
        test_db.transaction(|tx, _: &mut ()| Box::pin(async move {
            tx.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&owner, &station]).await?;
            tx.execute("UPDATE game.items SET location_id=$2, flag_id=$3 WHERE item_id=$1", &[&protected, &station, &FLAG_HANGAR]).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        c.session.apply(vec![("stationid", Value::Int(station))]);
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        let refusal = super::super::board(&mut c, protected, Some(old)).await;
        assert!(matches!(refusal, Err(crate::services::CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CANT_BOARD_SHIP_NOT_IN_HANGAR));
        test_db.transaction(|tx, _: &mut ()| Box::pin(async move {
            tx.execute("UPDATE game.characters SET station_id=NULL WHERE character_id=$1", &[&owner]).await?;
            tx.execute("UPDATE game.items SET location_id=$2, flag_id=0 WHERE item_id=$1", &[&protected, &system]).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn other_pilot_lock_refuses_foreign_board_not_the_owner() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (locker_account, locker) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let own = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let foreign = creation::positioned(&test_db, kind, locker, system, pos).await.unwrap();
        let locker_ship = creation::positioned(&test_db, kind, locker, system, pos).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&locker, &locker_ship]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        train(&test_db, &sde, locker, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        world.system(system).await.unwrap();
        for (id, who, pilot) in [(old, owner, true), (own, owner, false), (foreign, locker, false), (locker_ship, locker, true)] {
            let row = FreeShip { item_id: id, type_id: kind, owner_id: who, corporation_id: 1, alliance_id: -1, pos };
            let prepared = if pilot { balls::piloted(&test_db, sde.clone(), &row, who, 0.0).await.unwrap() } else { balls::unpiloted(&test_db, sde.clone(), &row).await.unwrap() };
            world.with(system, move |s| if pilot { prepared.install(s, |s, ball| s.add_ship(ball)); } else { prepared.install(s, |s, ball| s.add_persistent_ship(ball)); }).await.unwrap();
        }
        let mut boarder = client(test_db.clone(), sde.clone(), world.clone(), account).await;
        boarder.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&world, owner, boarder.notify_tx.clone());
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&boarder.session));
        let mut locking = client(test_db.clone(), sde.clone(), world.clone(), locker_account).await;
        locking.session.apply(vec![("userid", Value::Int(locker_account)), ("charid", Value::Int(locker)), ("shipid", Value::Int(locker_ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&world, locker, locking.notify_tx.clone());
        world.set_online_session(locker, infinity_core::session::OnlineSession::from_session(&locking.session));
        crate::services::combat::targeting_charges::add_target(&mut locking, foreign).await.unwrap();
        crate::services::combat::targeting_charges::add_target(&mut locking, own).await.unwrap();
        let due = world.with(system, move |s| {
            let scan = s.with_combat_fit(locker_ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, infinity_core::consts::dogma::ATTR_SCAN_RESOLUTION)).unwrap_or(1.0);
            let signature = sde.types.get(kind).and_then(|ty| ty.attribute(infinity_core::consts::dogma::ATTR_SIGNATURE_RADIUS)).unwrap_or(1.0);
            s.now_ms() + i64::try_from(crate::space::lock_time_ms(scan, signature)).unwrap_or(i64::MAX)
        }).await.unwrap();
        world.with(system, move |s| s.tick(due)).await.unwrap();
        assert!(crate::services::combat::targeting_charges::has_target(locker, foreign));
        assert!(crate::services::combat::targeting_charges::has_target(locker, own));
        let refused = super::super::board(&mut boarder, foreign, Some(old)).await;
        assert!(matches!(&refused, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict.as_slice() == [("notify".into(), marshal::Value::unicode(infinity_core::messages::ship::SHIP_LOCKED_BY_OTHER))]), "unexpected boarding refusal: {refused:?}");
        fourtuple(super::super::board(&mut boarder, own, Some(old)).await.expect("owner boards despite another pilot's lock"));
    }).await;
    cleanup(&db, locker_account, locker).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_refunds_a_live_reload_reservation_into_the_taken_hull() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let stack = crate::dao::items::transfers::insert(&test_db, charge, foreign_owner, target, infinity_core::consts::FLAG_CARGO, 100).await.unwrap();
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, stack,
            infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, target).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let pilot = balls::piloted(&test_db, sde.clone(), &FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos }, owner, 0.0).await.unwrap();
        let parked = balls::unpiloted(&test_db, sde.clone(), &FreeShip { item_id: target, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| { pilot.install(s, |s, ball| s.add_ship(ball)); parked.install(s, |s, ball| s.add_persistent_ship(ball)); }).await.unwrap();
        let mut c = client(test_db.clone(), sde, world, account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&c.world, owner, c.notify_tx.clone());
        c.world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        fourtuple(super::super::board(&mut c, target, Some(old)).await.ok().expect("foreign Board"));
        let refund = test_db.query_one("SELECT owner_id, quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap();
        assert_eq!(refund.get::<_, i64>("owner_id"), owner, "the refund stack stays in the taken hull");
        assert_eq!(refund.get::<_, i64>("quantity"), 140, "the reservation returns into its stack");
        assert!(test_db.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap().is_empty());
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_refunds_a_station_reservation_without_seizing_it() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|row| row.solar_system_id.is_some()).expect("SDE station");
    let (station, system) = (station.station_id, station.solar_system_id.expect("station system"));
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, target,
            infinity_core::consts::FLAG_CARGO, charge, 40, FLAG_HANGAR, station).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let pilot = balls::piloted(&test_db, sde.clone(), &FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos }, owner, 0.0).await.unwrap();
        let parked = balls::unpiloted(&test_db, sde.clone(), &FreeShip { item_id: target, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| { pilot.install(s, |s, ball| s.add_ship(ball)); parked.install(s, |s, ball| s.add_persistent_ship(ball)); }).await.unwrap();
        let mut c = client(test_db.clone(), sde, world, account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&c.world, owner, c.notify_tx.clone());
        c.world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        fourtuple(super::super::board(&mut c, target, Some(old)).await.ok().expect("foreign Board"));
        let created = test_db.query("SELECT quantity FROM game.items WHERE location_id=$1 AND flag_id=$2 AND type_id=$3 AND owner_id=$4", &[&station, &FLAG_HANGAR, &charge, &foreign_owner]).await.unwrap();
        assert_eq!(created.len(), 1, "the refund row is created outside the hull");
        assert_eq!(created[0].get::<_, i64>("quantity"), 40);
        assert!(test_db.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap().is_empty());
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_rolls_back_a_protected_refund_child_and_restores_the_reservation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let unit = |type_id: i32| sde.types.get(type_id).and_then(|t| t.volume).filter(|value| value.is_finite() && *value > 0.0);
    let admits = |type_id: i32, room: f64| unit(type_id).is_some_and(|value| value <= room)
        && crate::services::inventory::holds::check(&sde, infinity_core::consts::FLAG_CARGO, type_id, false, 1, crate::services::inventory::holds::CapacityLimit::Finite(room), 0.0).is_ok();
    let selected = sde.types.iter().filter(|ship| ship.category_id == Some(CATEGORY_SHIP) && ship.published == Some(true) && balls::radius(&sde, ship.type_id).is_ok())
        .filter_map(|ship| {
            let room = ship.attribute(infinity_core::consts::dogma::ATTR_CAPACITY).or(ship.capacity).filter(|value| value.is_finite() && *value > 0.0)?;
            let protected = sde.types.iter().filter(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && !sde.is_tradable(t.type_id) && admits(t.type_id, room))
                .min_by(|a, b| unit(a.type_id).unwrap().total_cmp(&unit(b.type_id).unwrap()))?;
            let protected_unit = unit(protected.type_id).unwrap();
            let charge = sde.types.iter().filter(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id) && admits(t.type_id, room))
                .filter_map(|t| {
                    let charge_unit = unit(t.type_id).unwrap();
                    let needed = (protected_unit / charge_unit).ceil();
                    let quantity = (needed.is_finite() && needed >= 1.0 && needed <= i64::MAX as f64).then_some(needed as i64).filter(|quantity| *quantity >= 1 && *quantity <= infinity_core::consts::MAX_STACK_QUANTITY - 100)?;
                    let expected = 100_i64.checked_add(quantity).filter(|stack| *stack <= infinity_core::consts::MAX_STACK_QUANTITY)?;
                    let occupied = 100.0 * charge_unit;
                    let cost = charge_unit * quantity as f64;
                    (occupied.is_finite() && cost.is_finite() && occupied <= room && occupied + cost <= room && cost >= protected_unit).then_some((t.type_id, charge_unit, quantity, expected))
                }).min_by(|a, b| a.1.total_cmp(&b.1))?;
            Some((ship.type_id, protected.type_id, charge.0, protected_unit, charge.1, charge.2, charge.3))
        }).min_by(|a, b| (100.0 * a.4 + a.4 * a.5 as f64).total_cmp(&(100.0 * b.4 + b.4 * b.5 as f64))).expect("hull with eligible cargo units");
    let (kind, protected_type, charge, protected_unit, charge_unit, control_quantity, expected_stack) = (selected.0, selected.1, selected.2, selected.3, selected.4, selected.5, selected.6);
    let control_cost = charge_unit * control_quantity as f64;
    assert!(control_cost.is_finite() && control_cost >= protected_unit, "the control deposit costs at least the protected unit");
    assert!(sde.is_tradable(charge) && !sde.is_tradable(protected_type));
    assert_eq!(expected_stack, 100 + control_quantity);
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let stack = crate::dao::items::transfers::insert(&test_db, charge, foreign_owner, target, infinity_core::consts::FLAG_CARGO, 100).await.unwrap();
        // Same hull; the control quantity costs at least this protected unit.
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, stack,
            infinity_core::consts::FLAG_CARGO, protected_type, 1, infinity_core::consts::FLAG_CARGO, target).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let pilot = balls::piloted(&test_db, sde.clone(), &FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos }, owner, 0.0).await.unwrap();
        let parked = balls::unpiloted(&test_db, sde.clone(), &FreeShip { item_id: target, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| { pilot.install(s, |s, ball| s.add_ship(ball)); parked.install(s, |s, ball| s.add_persistent_ship(ball)); }).await.unwrap();
        let mut c = client(test_db.clone(), sde, world, account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        crate::space::session_change::test_support::register_elapsed(&c.world, owner, c.notify_tx.clone());
        c.world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        assert!(super::super::board(&mut c, target, Some(old)).await.is_err(), "a protected refund child refuses the whole take");
        let restored = test_db.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap();
        assert_eq!(restored.len(), 1, "rollback keeps the captured reservation");
        assert!(test_db.query("SELECT item_id FROM game.items WHERE location_id=$1 AND type_id=$2", &[&target, &protected_type]).await.unwrap().is_empty(), "no protected child survives");
        let row = test_db.query_one("SELECT owner_id, quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap();
        assert_eq!((row.get::<_, i64>("owner_id"), row.get::<_, i64>("quantity")), (foreign_owner, 100));
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(old));
        // Same hull and preimage. The control costs at least the refused unit,
        // including the hundred cargo units already in the hold.
        test_db.execute("UPDATE game.pending_reloads SET type_id=$2, quantity=$3 WHERE ship_id=$1", &[&target, &charge, &control_quantity]).await.unwrap();
        fourtuple(super::super::board(&mut c, target, Some(old)).await.ok().expect("the capacity-matched control boards"));
        let control = test_db.query_one("SELECT owner_id, quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap();
        assert_eq!((control.get::<_, i64>("owner_id"), control.get::<_, i64>("quantity")), (owner, expected_stack));
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

/// Install both hulls for one foreign Board and return the ready caller.
async fn board_scene(test_db: &Db, sde: &Arc<Sde>, system: i64, kind: i32, account: i64, owner: i64,
    old: i64, target: i64, foreign_owner: i64, pos: [f64; 3]) -> (Client, Arc<World>) {
    let world = World::new(test_db.clone(), sde.clone(), 1);
    let pilot = balls::piloted(test_db, sde.clone(), &FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos }, owner, 0.0).await.unwrap();
    let parked = balls::unpiloted(test_db, sde.clone(), &FreeShip { item_id: target, type_id: kind, owner_id: foreign_owner, corporation_id: 1, alliance_id: -1, pos }).await.unwrap();
    world.system(system).await.unwrap();
    world.with(system, move |s| { pilot.install(s, |s, ball| s.add_ship(ball)); parked.install(s, |s, ball| s.add_persistent_ship(ball)); }).await.unwrap();
    let mut c = client(test_db.clone(), sde.clone(), world.clone(), account).await;
    c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
    crate::space::session_change::test_support::register_elapsed(&c.world, owner, c.notify_tx.clone());
    world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
    (c, world)
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_spills_an_external_refund_stack_without_seizing_it() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|row| row.solar_system_id.is_some()).expect("SDE station");
    let (station, system) = (station.station_id, station.solar_system_id.expect("station system"));
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let full = infinity_core::consts::MAX_STACK_QUANTITY - 10;
        let stack = crate::dao::items::transfers::insert(&test_db, charge, foreign_owner, station, FLAG_HANGAR, full).await.unwrap();
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, target,
            infinity_core::consts::FLAG_CARGO, charge, 40, FLAG_HANGAR, station).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let (mut c, world) = board_scene(&test_db, &sde, system, kind, account, owner, old, target, foreign_owner, pos).await;
        fourtuple(super::super::board(&mut c, target, Some(old)).await.ok().expect("foreign Board"));
        drop(world);
        let mut rows: Vec<(i64, i64)> = test_db.query("SELECT item_id, quantity FROM game.items WHERE location_id=$1 AND flag_id=$2 AND type_id=$3 AND owner_id=$4 ORDER BY item_id", &[&station, &FLAG_HANGAR, &charge, &foreign_owner]).await.unwrap().iter().map(|row| (row.get("item_id"), row.get("quantity"))).collect();
        rows.sort_unstable();
        assert_eq!(rows.len(), 2, "one merged stack and one spilled stack leave on the original owner");
        assert_eq!(rows[0], (stack, infinity_core::consts::MAX_STACK_QUANTITY));
        assert_eq!(rows[1].1, 30, "the remainder spills into a new external stack");
        assert_eq!(i128::from(rows[0].1) + i128::from(rows[1].1), i128::from(full) + 40, "the reservation is conserved across the merged and spilled rows");
        assert!(test_db.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap().is_empty());
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_board_refunds_a_missing_source_into_the_taken_hull() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let gone = i64::MAX - 1;
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, target,
            infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, gone).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let (mut c, world) = board_scene(&test_db, &sde, system, kind, account, owner, old, target, foreign_owner, pos).await;
        fourtuple(super::super::board(&mut c, target, Some(old)).await.ok().expect("foreign Board"));
        drop(world);
        let row = test_db.query_one("SELECT owner_id, quantity FROM game.items WHERE location_id=$1 AND flag_id=$2 AND type_id=$3", &[&target, &infinity_core::consts::FLAG_CARGO, &charge]).await.unwrap();
        assert_eq!(row.get::<_, i64>("owner_id"), owner, "the fallback deposit lands in the taken hull");
        assert_eq!(row.get::<_, i64>("quantity"), 40);
        assert!(test_db.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap().is_empty());
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_attempt_proof_reports_unknown_for_extra_missing_or_modified_state() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..cfg.clone() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('proof-unknown-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,'proof-unknown-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let foreign_owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,'proof-unknown-other-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let old: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($2,$1,1,0,1,true,0,0,0) RETURNING item_id", &[&owner, &kind]).await.unwrap().get("item_id");
    let target: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($2,$1,1,0,1,true,0,0,0) RETURNING item_id", &[&foreign_owner, &kind]).await.unwrap().get("item_id");
    let stack = crate::dao::items::transfers::insert(&tx, charge, foreign_owner, target, infinity_core::consts::FLAG_CARGO, 100).await.unwrap();
    crate::dao::items::pending_reloads::insert(&tx, foreign_owner, target, target,
        infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, target).await.unwrap();
    tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
    let preparation = crate::services::combat::reload_recovery::TakePreparation::capture(&tx, &sde, target).await.unwrap();
    let mut refund_ids = preparation.refund().candidate_ids();
    refund_ids.extend(preparation.refund().mandatory_ids());
    refund_ids.extend(preparation.pending().iter().map(|row| row.source_location_id));
    let proof = crate::dao::items::boarding::ownership::BoardOwnershipProof::capture(&tx, target, &refund_ids).await.unwrap();
    let plan = crate::dao::items::boarding::RecoveryPlan::Board { character: owner, user: account, system: 1, old, old_type: kind,
        target, target_type: kind, ownership: Some(std::sync::Arc::new(std::sync::Mutex::new(Some(proof)))) };
    assert!(matches!(crate::dao::items::boarding::recovery(&tx, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Aborted));
    let extra = tx.savepoint("extra").await.unwrap();
    extra.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false)", &[&charge, &foreign_owner, &target, &infinity_core::consts::FLAG_CARGO]).await.unwrap();
    assert!(matches!(crate::dao::items::boarding::recovery(&extra, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "an extra child contradicts the captured closure");
    extra.rollback().await.unwrap();
    let missing = tx.savepoint("missing").await.unwrap();
    missing.execute("DELETE FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap();
    assert!(matches!(crate::dao::items::boarding::recovery(&missing, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "a missing member contradicts the captured closure");
    missing.rollback().await.unwrap();
    let modified = tx.savepoint("modified").await.unwrap();
    modified.execute("UPDATE game.items SET quantity=101 WHERE item_id=$1", &[&stack]).await.unwrap();
    assert!(matches!(crate::dao::items::boarding::recovery(&modified, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "a modified member contradicts the captured image");
    modified.rollback().await.unwrap();
    assert!(matches!(crate::dao::items::boarding::recovery(&tx, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Aborted));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn foreign_attempt_fresh_copy_inherits_no_refund_effects() {
    use crate::dao::items::parent_locks::{LockRequest, LockStrength};
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..cfg.clone() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('proof-fresh-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,'proof-fresh-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let foreign_owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id) VALUES ($1,'proof-fresh-other-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let target: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($2,$1,1,0,1,true,0,0,0) RETURNING item_id", &[&foreign_owner, &kind]).await.unwrap().get("item_id");
    crate::dao::items::transfers::insert(&tx, charge, foreign_owner, target, infinity_core::consts::FLAG_CARGO, 100).await.unwrap();
    crate::dao::items::pending_reloads::insert(&tx, foreign_owner, target, target,
        infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, target).await.unwrap();
    let mut preparation = crate::services::combat::reload_recovery::TakePreparation::capture(&tx, &sde, target).await.unwrap();
    let mut requests: Vec<_> = preparation.closure().iter().copied()
        .map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    requests.extend_from_slice(preparation.refund().requests());
    let mut optional = preparation.refund().candidate_ids();
    optional.retain(|id| !requests.iter().any(|request| request.item_id == *id));
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &optional).await.unwrap();
    let mut outbox = crate::db::outbox::Outbox::<()>::default();
    let receipts = crate::services::combat::reload_recovery::settle_subtree(&tx, &sde, &mut outbox, target, target, owner, None, &locks, &mut preparation).await.unwrap();
    assert_eq!(receipts.len(), 1, "the producer returns the real refund batch");
    preparation.record_receipts(receipts);
    let retained: Vec<_> = preparation.take_receipts().iter().flat_map(|receipt| receipt.rows()).map(|row| (row.item.item_id, row.item.quantity, row.old_quantity)).collect();
    assert_eq!(retained.len(), 1, "the recorded receipt keeps the produced stack");
    let mut fresh = preparation.fresh_attempt();
    assert!(fresh.take_receipts().is_empty(), "a fresh attempt inherits no refund effects");
    assert_eq!(fresh.closure(), preparation.closure());
    let again = crate::services::combat::reload_recovery::settle_subtree(&tx, &sde, &mut outbox, target, target, owner, None, &locks, &mut fresh).await;
    assert!(again.is_err(), "the consumed reservation gives the next attempt nothing to settle");
    assert!(fresh.take_receipts().is_empty(), "the failed next attempt records no receipt");
    assert!(preparation.take_receipts().is_empty(), "reading the recorded receipts moves them out once");
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn admitted_foreign_board_commits_after_the_caller_drops() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let cfg = crate::config::tests::test_database();
    let (gate, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let external = creation::positioned(&test_db, kind, foreign_owner, system, [pos[0] + 20_000.0, 0.0, 0.0]).await.unwrap();
        let stack = crate::dao::items::transfers::insert(&test_db, charge, foreign_owner, external, infinity_core::consts::FLAG_CARGO, 10).await.unwrap();
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, target, infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, external).await.unwrap();
        crate::dao::insurance::insure(&test_db, target, foreign_owner, kind, 0.5, 1, i64::MAX).await.unwrap();
        crate::dao::insurance::insure(&test_db, old, owner, kind, 0.5, 1, i64::MAX).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let (c, world) = board_scene(&test_db, &sde, system, kind, account, owner, old, target, foreign_owner, pos).await;
        let expected_online = world.online_session(owner).expect("admitted online identity");
        let client = std::sync::Arc::new(tokio::sync::Mutex::new(c));
        gate.batch_execute("BEGIN").await.unwrap();
        let gate_pid: i32 = gate.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        gate.query_one("SELECT character_id FROM game.characters WHERE character_id=$1 FOR UPDATE", &[&owner]).await.unwrap();
        let caller_client = std::sync::Arc::clone(&client);
        let caller = tokio::spawn(async move { super::super::board(&mut *caller_client.lock().await, target, Some(old)).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                gate.batch_execute("SELECT pg_stat_clear_snapshot()").await.expect("refresh admission activity snapshot");
                let waiting: bool = gate.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid<>pg_backend_pid() AND wait_event_type='Lock' AND $1 = ANY(pg_blocking_pids(pid)) AND query LIKE 'SELECT character_id FROM game.characters WHERE character_id = $1 FOR UPDATE%') AS waiting", &[&gate_pid]).await.unwrap().get("waiting");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("admitted worker never blocked on this gate");
        caller.abort();
        gate.batch_execute("ROLLBACK").await.unwrap();
        assert!(caller.await.unwrap_err().is_cancelled(), "the caller drops after admission");
        assert_eq!(world.online_session(owner), Some(expected_online), "caller cancellation keeps the connected online identity");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if world.ball(system, target).await.is_some_and(|ball| ball.owner_id == owner && ball.char_id == Some(owner)) { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("admitted worker did not publish the committed ball");
        let hull = test_db.query_one("SELECT owner_id, location_id FROM game.items WHERE item_id=$1", &[&target]).await.unwrap();
        assert_eq!((hull.get::<_, i64>("owner_id"), hull.get::<_, i64>("location_id")), (owner, system));
        let external_row = test_db.query_one("SELECT owner_id, location_id, quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap();
        assert_eq!((external_row.get::<_, i64>("owner_id"), external_row.get::<_, i64>("location_id"), external_row.get::<_, i64>("quantity")), (foreign_owner, external, 50));
        assert!(test_db.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap().is_empty());
        assert!(crate::dao::insurance::contract(&test_db, target).await.unwrap().is_none());
        assert!(crate::dao::insurance::contract(&test_db, old).await.unwrap().is_some());
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(target));
        let seen = world.with(system, move |s| (
            s.pilot_ball(owner).map(|ball| (ball.id, ball.owner_id, ball.char_id)),
            s.ball(old).map(|ball| ball.char_id),
            s.ball(target).map(|ball| (ball.owner_id, ball.char_id)),
        )).await.unwrap();
        assert_eq!(seen.0, Some((target, owner, Some(owner))));
        assert_eq!(seen.1, Some(None));
        assert_eq!(seen.2, Some((owner, Some(owner))));
        let retained = world.with(system, move |s| (
            s.pilot_ball(owner).map(|ball| (ball.id, ball.owner_id, ball.corp_id, ball.alliance_id, ball.char_id)),
            s.ball(old).map(|ball| ball.char_id),
            (s.combat_phase(old), s.combat_phase(target)),
        )).await.unwrap();
        assert_eq!(retained.0, Some((target, owner, 1, -1, Some(owner))));
        assert_eq!(retained.1, Some(None));
        assert_eq!(retained.2, (Some(crate::space::combat::CombatPhase::Live), Some(crate::space::combat::CombatPhase::Live)), "the committed worker left both hulls live");
        assert_eq!(test_db.query_one("SELECT owner_id, quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap().get::<_, i64>("quantity"), 50);
        drop(world);
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn post_refund_recovery_matches_commit_or_stays_unknown() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true) && balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let charge = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("tradable charge").type_id;
    let other = sde.types.iter().find(|t| t.type_id != charge && t.category_id != Some(CATEGORY_SHIP) && t.published == Some(true) && sde.is_tradable(t.type_id)).expect("second tradable type").type_id;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let (foreign_account, foreign_owner) = fixture(&db, &sde, system, None).await;
    let cfg = crate::config::tests::test_database();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [0.0, 0.0, 0.0];
        let old = creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        let target = creation::positioned(&test_db, kind, foreign_owner, system, pos).await.unwrap();
        let external = creation::positioned(&test_db, kind, foreign_owner, system, [20_000.0, 0.0, 0.0]).await.unwrap();
        let stack = crate::dao::items::transfers::insert(&test_db, charge, foreign_owner, external, infinity_core::consts::FLAG_CARGO, infinity_core::consts::MAX_STACK_QUANTITY).await.unwrap();
        crate::dao::items::pending_reloads::insert(&test_db, foreign_owner, target, target, infinity_core::consts::FLAG_CARGO, charge, 40, infinity_core::consts::FLAG_CARGO, external).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await.unwrap();
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let identity = crate::services::identity::Identity { char_id: owner, ship_id: Some(old), corp_id: 1, station_id: None, system_id: Some(system) };
        let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let mut tx = client.transaction().await.unwrap();
        let mut outbox = crate::db::outbox::Outbox::<()>::default();
        let mut produced = tx.savepoint("produced").await.unwrap();
        let (plan, created_ids) = crate::dao::items::boarding::RecoveryPlan::foreign_proof_for_test(&test_db, &world, &produced, &sde, &mut outbox, &identity, account, system, old, kind, target, kind, pos, pos, &[], None).await.unwrap();
        let created = *created_ids.first().expect("the transfer creates one refund row");
        assert_eq!(produced.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&created]).await.unwrap().get::<_, i64>("quantity"), 40);
        assert!(matches!(crate::dao::items::boarding::recovery(&produced, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Committed { .. }), "the transfer's sealed postimage is a complete commit");
        assert!(!outbox.into_entries().is_empty(), "the transfer queued its publications");
        let extra = produced.savepoint("extra").await.unwrap();
        extra.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false)", &[&other, &owner, &target, &infinity_core::consts::FLAG_CARGO]).await.unwrap();
        assert!(matches!(crate::dao::items::boarding::recovery(&extra, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "an extra child is unknown");
        extra.rollback().await.unwrap();
        let missing = produced.savepoint("missing").await.unwrap();
        missing.execute("DELETE FROM game.items WHERE item_id=$1", &[&created]).await.unwrap();
        assert!(matches!(crate::dao::items::boarding::recovery(&missing, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "a missing created refund row is unknown");
        missing.rollback().await.unwrap();
        let modified = produced.savepoint("modified").await.unwrap();
        modified.execute("UPDATE game.items SET quantity=quantity-1 WHERE item_id=$1", &[&stack]).await.unwrap();
        assert!(matches!(crate::dao::items::boarding::recovery(&modified, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "a modified external stack is unknown");
        modified.rollback().await.unwrap();
        let policy = produced.savepoint("policy").await.unwrap();
        crate::dao::insurance::insure(&policy, target, owner, kind, 0.5, 1, i64::MAX).await.unwrap();
        assert!(matches!(crate::dao::items::boarding::recovery(&policy, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "an added policy is unknown");
        policy.rollback().await.unwrap();
        assert!(matches!(crate::dao::items::boarding::recovery(&produced, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Committed { .. }));
        produced.rollback().await.unwrap();
        assert!(tx.query("SELECT item_id FROM game.items WHERE item_id=$1", &[&created]).await.unwrap().is_empty(), "transfer rollback removes the created refund row");
        assert_eq!(tx.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&stack]).await.unwrap().get::<_, i64>("quantity"), infinity_core::consts::MAX_STACK_QUANTITY);
        assert_eq!(tx.query("SELECT reload_id FROM game.pending_reloads WHERE ship_id=$1", &[&target]).await.unwrap().len(), 1, "transfer rollback restores the reservation");
        let failed = tx.savepoint("failed").await.unwrap();
        failed.execute("UPDATE game.items SET quantity=quantity-1 WHERE item_id=$1", &[&stack]).await.unwrap();
        let mut failed_outbox = crate::db::outbox::Outbox::<()>::default();
        let failed_proof = crate::dao::items::boarding::RecoveryPlan::foreign_proof_for_test(&test_db, &world, &failed, &sde, &mut failed_outbox, &identity, account, system, old, kind, target, kind, pos, pos, &[], None).await;
        assert!(failed_proof.is_err(), "a modified source refuses the transfer");
        assert!(matches!(crate::dao::items::boarding::recovery(&failed, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Unknown), "the failed transfer keeps the image unknown");
        assert!(failed_outbox.into_entries().is_empty(), "the refused transfer publishes nothing");
        failed.rollback().await.unwrap();
        assert!(matches!(crate::dao::items::boarding::recovery(&tx, &plan).await.unwrap(), crate::dao::items::boarding::Recovery::Aborted), "the restored preimage is aborted");
        assert!(tx.query("SELECT item_id FROM game.items WHERE item_id=$1", &[&created]).await.unwrap().is_empty(), "the created refund row stays absent");
        tx.rollback().await.unwrap();
        drop(world);
    }).await;
    cleanup(&db, foreign_account, foreign_owner).await;
    cleanup(&db, account, owner).await;
    result.unwrap();
}
