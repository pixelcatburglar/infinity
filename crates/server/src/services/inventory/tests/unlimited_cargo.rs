//! Unlimited-cargo admission races across two real transactions.
//! A captured hold token must refuse after a committed capacity-mode change,
//! and goods written under an earlier committed token survive a later reset.

use super::*;
use crate::dao::items::{dogma as dogma_dao, parent_locks::tests::{backend_pid, wait_blocked}, ship::lock_reload_ship};
use crate::services::inventory::hold_storage::{CapacitySource, HoldCapacity};
use std::sync::Arc;

const CAP: i32 = infinity_core::consts::dogma::ATTR_CAPACITY;
/// A singleton hull parked at a station hangar through the real creation path.
async fn stowed_ship(db: &crate::db::Db, sde: &Arc<Sde>, owner: i64) -> (i64, i64, f64) {
    let station = sde.map.stations().next().unwrap().station_id;
    let (ship_type, base) = sde.types.iter()
        .filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP))
        .find_map(|ty| crate::services::inventory::hold_storage::static_capacity(sde, ty.type_id).ok()
            .filter(|v| v.is_finite() && *v > 0.0).map(|v| (ty.type_id, v)))
        .expect("SDE must contain a ship with cargo capacity");
    let ship = crate::dao::items::creation::rookie_ship(db, ship_type, owner, station, FLAG_HANGAR).await.unwrap();
    (ship, station, base)
}

/// The smallest positive-volume type one unit of which honestly fits the
/// finite quota, so a stale-witness refusal cannot be mistaken for a quota one.
fn unit_cargo(sde: &Sde, base: f64) -> i32 {
    sde.types.iter()
        .filter(|ty| ty.published == Some(true))
        .filter_map(|ty| {
            let unit = crate::services::item_volume::item_volume(sde, ty.type_id, false, 1, None).ok()?;
            (unit.is_finite() && unit > 0.0 && unit <= base && unit * infinity_core::consts::MAX_STACK_QUANTITY as f64 > base
                && crate::services::inventory::holds::check(sde, FLAG_CARGO, ty.type_id, false, 1,
                    crate::services::inventory::holds::CapacityLimit::Finite(base), 0.0).is_ok())
            .then_some((ty.type_id, unit))
        })
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(type_id, _)| type_id)
        .expect("SDE must contain a cargo type whose unit fits the hull")
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn stale_captured_capacity_refuses_after_reset_and_changes_nothing() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        for (name, begin_unlimited, change) in [
            ("unlimited→same-base finite", true, 0),
            ("unlimited→reset absence", true, 1),
            ("finite→same-base unlimited", false, 2),
        ] {
            let (ship, _station, base) = stowed_ship(&db, &sde, actor.character).await;
            if begin_unlimited { dogma_dao::set_cargo_unlimited(&db, ship, base).await.unwrap(); }
            else { dogma_dao::set_override(&db, ship, CAP, base).await.unwrap(); }
            let kind = unit_cargo(&sde, base);
            let fit = crate::services::fit::Fit::load_unpiloted(&db, sde.clone(), ship).await.unwrap();
            let source = CapacitySource::Captured(HoldCapacity::capture(&fit, FLAG_CARGO).unwrap());
            let (mut holder_client, holder_conn) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
            tokio::spawn(async move { holder_conn.await.unwrap(); });
            let holder = holder_client.transaction().await.unwrap();
            assert!(lock_reload_ship(&holder, ship).await.unwrap());
            match change {
                0 => { dogma_dao::set_override(&holder, ship, CAP, base).await.unwrap(); }
                1 => { dogma_dao::clear_override(&holder, ship, CAP).await.unwrap(); }
                _ => { dogma_dao::set_cargo_unlimited(&holder, ship, base).await.unwrap(); }
            }
            let pid = backend_pid(&holder).await;
            let dispatched = std::sync::atomic::AtomicBool::new(false);
            let worker = db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let sde = sde.clone();
                    let destination = crate::services::inventory::destination::Destination::owned(actor.character, ship, FLAG_CARGO);
                    Box::pin(async move {
                        crate::services::items::deposit_with_capacity(tx, &sde, outbox, &destination, kind, 1, 0,
                            Default::default(), source).await
                    })
                },
                |value, _| async {
                    dispatched.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok::<_, CallError>(value)
                },
            );
            let (result, _) = tokio::join!(worker, async {
                wait_blocked(&db, pid).await;
                holder.commit().await.unwrap();
            });
            let Err(CallError::User { msg, dict }) = result else { panic!("{name}: expected stale user error") };
            assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
            assert!(dict.iter().any(|(k, v)| k.as_str() == "notify"
                && v.as_text().as_deref() == Some(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION)));
            assert!(!dispatched.load(std::sync::atomic::Ordering::Relaxed));
            assert!(crate::dao::items::loading::load_items(&db, ItemQuery::LocationFlags(ship, &[FLAG_CARGO])).await.unwrap().is_empty(),
                "{name}: refused deposit leaves no inventory rows");
        }
    }).await;
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    result.expect("stale-token race failed");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn committed_deposit_survives_reset_while_later_reset_waits_on_hold_lock() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let actor = crate::services::tests::fixture::create(&db).await.unwrap();
    let result = crate::tests::catch_scenario(async {
        let (ship, _station, base) = stowed_ship(&db, &sde, actor.character).await;
        dogma_dao::set_cargo_unlimited(&db, ship, base).await.unwrap();
        let kind = unit_cargo(&sde, base);
        let fit = crate::services::fit::Fit::load_unpiloted(&db, sde.clone(), ship).await.unwrap();
        let source = CapacitySource::Captured(HoldCapacity::capture(&fit, FLAG_CARGO).unwrap());
        let (mut holder_client, holder_conn) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { holder_conn.await.unwrap(); });
        let holder = holder_client.transaction().await.unwrap();
        let mut outbox = crate::db::outbox::Outbox::default();
        let world = crate::space::World::new(db.clone(), sde.clone(), 1);
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        assert!(crate::services::inventory::holds::check(&sde, FLAG_CARGO, kind, false,
            infinity_core::consts::MAX_STACK_QUANTITY, crate::services::inventory::holds::CapacityLimit::Finite(base), 0.0).is_err());
        assert!(crate::services::inventory::holds::check(&sde, FLAG_CARGO, kind, false,
            infinity_core::consts::MAX_STACK_QUANTITY, crate::services::inventory::holds::CapacityLimit::Unlimited, 0.0).is_ok());
        let destination = crate::services::inventory::destination::Destination::owned(actor.character, ship, FLAG_CARGO);
        let receipt = crate::services::items::deposit_with_capacity(&holder, &sde, &mut outbox, &destination, kind,
            infinity_core::consts::MAX_STACK_QUANTITY, 0, Default::default(), source).await.unwrap();
        let deposited = receipt.last().item_id;
        let quantity = receipt.last().quantity;
        assert_eq!(quantity, infinity_core::consts::MAX_STACK_QUANTITY);
        assert!(events.try_recv().is_err());
        let pid = backend_pid(&holder).await;
        let resetter = db.transaction(
            |tx, _: &mut ()| Box::pin(async move {
                assert!(lock_reload_ship(tx, ship).await.map_err(crate::services::db_err)?);
                dogma_dao::clear_override(tx, ship, CAP).await.map_err(crate::services::db_err)?;
                Ok::<(), CallError>(())
            }),
            |(), _| async move { Ok::<(), CallError>(()) },
        );
        let (_, reset) = tokio::join!(async {
            wait_blocked(&db, pid).await;
            holder.commit().await.unwrap();
            crate::db::outbox::dispatch_sender((), outbox, &world, Some(&sender)).unwrap();
        }, resetter);
        reset.unwrap();
        assert!(events.try_recv().is_ok(), "committed deposit dispatches its notification");
        let row = crate::dao::items::loading::load_items(&db, ItemQuery::Id(deposited)).await.unwrap().pop().unwrap();
        assert_eq!((row.location_id, row.flag_id, row.quantity), (ship, FLAG_CARGO, quantity),
            "the committed deposit's real cargo survives the reset");
        assert!(dogma_dao::overrides(&db, ship).await.unwrap().iter().all(|row| row.attribute_id != CAP),
            "reset removes the capacity-mode row");
        assert!(!crate::services::fit::Fit::load_unpiloted(&db, sde.clone(), ship).await.unwrap().cargo_unlimited());
    }).await;
    crate::services::tests::fixture::cleanup(&db, actor).await.unwrap();
    result.expect("surviving-deposit race failed");
}
