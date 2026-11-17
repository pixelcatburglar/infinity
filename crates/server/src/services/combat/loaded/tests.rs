//! DB-backed proofs for the loaded thin adapters: settle results, fresh
//! reseed outcomes and the resident/cold effective view.
use super::*;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipFixture, ShipItemSpec, ShipLocation};
use std::future::Future;

fn write(ship: i64, module: i64, flag: i32, type_id: i32, quantity: i64) -> LoadedWrite {
    LoadedWrite { ship, module, flag, type_id, incarnation: 0, quantity }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn settle_in_commits_absolute_and_reports_unmatched() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system: 910_003 }, &[
        ShipItemSpec { type_id: 1, flag_id: 27, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module = fixture.items[0];
    crate::dao::items::combat::apply_loads(&db, fixture.ship, &[(0, 27, 9, 8, 0)]).await.unwrap();
    settle_in(&db, &[write(fixture.ship, module, 27, 9, 7)]).await.unwrap();
    assert_eq!(charges::loaded(&db, fixture.ship, 27).await.unwrap().unwrap().quantity, 7);
    assert!(settle_in(&db, &[write(fixture.ship, module, 27, 8, 7)]).await.is_err(), "a type-mismatched row is an attempt failure");
    assert!(settle_in(&db, &[write(fixture.ship, module, 28, 9, 0)]).await.is_err(), "a delete that matched nothing is an attempt failure");
    assert_eq!(charges::loaded(&db, fixture.ship, 27).await.unwrap().unwrap().quantity, 7);
    cleanup_ship(&db, None, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn reseed_restores_durable_truth_and_failure_stays_held() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let system = 910_004_i64;
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system }, &[
        ShipItemSpec { type_id: 1, flag_id: 27, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (ship, module) = (fixture.ship, fixture.items[0]);
    crate::dao::items::combat::apply_loads(&db, ship, &[(0, 27, 9, 8, 0)]).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    world.with(system, move |s| {
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ship_type, vec![]);
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        s.install_condition(ship, crate::services::combat::groups_health::Condition {
            now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        let membership = s.combat_membership_generation(ship).unwrap();
        s.publish_loaded_absolute(ship, membership, vec![LoadedSeed {
            module, flag: 27, type_id: Some(10), quantity: 2, pending: false }]);
        s.invalidate_held_loaded(&[LoadedSlotKey { ship, module, flag: 27 }]);
    }).await.unwrap();
    assert!(reseed_held(&db, &world, 999_999, &[LoadedSlotKey { ship, module, flag: 27 }]).await.is_err());
    assert_eq!(world.with(system, |s| s.unknown_loaded_keys()).await.unwrap(), vec![LoadedSlotKey { ship, module, flag: 27 }],
        "a failed reseed keeps the slot held and unknown");
    reseed_held(&db, &world, system, &[LoadedSlotKey { ship, module, flag: 27 }]).await.unwrap();
    let view = world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten();
    assert_eq!(view.map(|v| (v.1, v.2)), Some((9, 8)), "a successful reseed restores durable truth");
    let effective = rows(&db, &world, Some(system), ship).await.unwrap();
    assert_eq!(effective.iter().map(|r| (r.flag_id, r.type_id, r.quantity)).collect::<Vec<_>>(), vec![(27, 9, 8)]);
    cleanup_ship(&db, Some(&*world), fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn reseed_drops_a_slot_whose_module_is_gone() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let system = 910_005_i64;
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system }, &[
        ShipItemSpec { type_id: 1, flag_id: 27, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (ship, module) = (fixture.ship, fixture.items[0]);
    crate::dao::items::combat::apply_loads(&db, ship, &[(0, 27, 9, 8, 0)]).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    world.system(system).await.unwrap();
    world.with(system, move |s| {
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ship_type, vec![]);
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        s.install_condition(ship, crate::services::combat::groups_health::Condition {
            now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        let membership = s.combat_membership_generation(ship).unwrap();
        s.publish_loaded_absolute(ship, membership, vec![LoadedSeed {
            module, flag: 27, type_id: Some(9), quantity: 8, pending: false }]);
        s.invalidate_held_loaded(&[LoadedSlotKey { ship, module, flag: 27 }]);
    }).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&module]).await.unwrap();
    let key = LoadedSlotKey { ship, module, flag: 27 };
    let entries = seed_entries(&db, ship, &[(module, 27)]).await.unwrap();
    assert!(matches!(&entries[..], [(entry, None)] if *entry == key),
        "a vanished module projects a removal, not an empty slot");
    reseed_held(&db, &world, system, &[key]).await.unwrap();
    assert!(world.with(system, move |s| s.unknown_loaded_keys()).await.unwrap().is_empty());
    assert!(world.with(system, move |s| s.loaded_effective(ship, module)).await.unwrap().is_none(),
        "the removed slot leaves no phantom charge");
    cleanup_ship(&db, Some(&*world), fixture).await.unwrap();
}

/// A resident ship whose durable load was invalidated by an armed boundary hold.
async fn unknown_slot(db: &Db, sde: &std::sync::Arc<crate::sde::Sde>, world: &World, system: i64) -> (ShipFixture, LoadedSlotKey) {
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let fixture = create_ship(db, ship_type, ShipLocation::Space { system }, &[
        ShipItemSpec { type_id: 1, flag_id: 27, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (ship, module) = (fixture.ship, fixture.items[0]);
    crate::dao::items::combat::apply_loads(db, ship, &[(0, 27, 9, 8, 0)]).await.unwrap();
    world.system(system).await.unwrap();
    let sde = sde.clone();
    world.with(system, move |s| {
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ship_type, vec![]);
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        s.install_condition(ship, crate::services::combat::groups_health::Condition {
            now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        let membership = s.combat_membership_generation(ship).unwrap();
        s.publish_loaded_absolute(ship, membership, vec![LoadedSeed {
            module, flag: 27, type_id: Some(9), quantity: 8, pending: false }]);
        s.invalidate_held_loaded(&[LoadedSlotKey { ship, module, flag: 27 }]);
    }).await.unwrap();
    (fixture, LoadedSlotKey { ship, module, flag: 27 })
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn reseed_waits_for_the_hull_lock_then_reads_committed_truth() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = 910_006_i64;
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let (fixture, key) = unknown_slot(&db, &sde, &world, system).await;
    let (mut client, connection) = crate::config::tests::test_database()
        .to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { let _ = connection.await; });
    let tx = client.transaction().await.unwrap();
    crate::dao::items::ship::lock_reload_ship(&tx, key.ship).await.unwrap();
    tx.execute("UPDATE game.module_charges SET quantity = $3 WHERE ship_id = $1 AND flag_id = $2",
        &[&key.ship, &key.flag, &3_i64]).await.unwrap();
    // The fresh read cannot race past the lock: every drive stays pending.
    let keys = [key];
    let mut fresh = std::pin::pin!(reseed_held(&db, &world, system, &keys));
    for _ in 0..100 {
        tokio::task::yield_now().await;
        std::future::poll_fn(|cx| {
            assert!(fresh.as_mut().poll(cx).is_pending(),
                "the fresh read must wait for the earlier hull lock");
            std::task::Poll::Ready(())
        }).await;
    }
    tx.commit().await.unwrap();
    fresh.await.unwrap();
    let view = world.with(system, move |s| s.loaded_effective(key.ship, key.module)).await.flatten();
    assert_eq!(view.map(|v| (v.1, v.2)), Some((9, 3)), "the fresh read observed the committed truth");
    cleanup_ship(&db, Some(&*world), fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn reconcile_gates_reseed_scope_before_recovery() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = 910_007_i64;
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let (fixture, key) = unknown_slot(&db, &sde, &world, system).await;
    assert!(fresh_gate(&db, &world, system, &[key.ship + 1]).await);
    assert_eq!(world.with(system, move |s| s.unknown_loaded_keys()).await.unwrap(), vec![key],
        "an out-of-scope unknown is left for its own boundary");
    // The dock/transfer/logout retry and the board/store reconcile share this gate.
    assert!(crate::space::system::boarding::pending_loaded_retry(&world, &db, system, key.ship, 0, 0).await,
        "the retry gate opens once the fresh read reseeds");
    assert_eq!(world.with(system, move |s| s.loaded_effective(key.ship, key.module)).await.flatten()
        .map(|v| (v.1, v.2)), Some((9, 8)), "recovery proceeds on the reseeded truth");
    cleanup_ship(&db, Some(&*world), fixture).await.unwrap();
}
