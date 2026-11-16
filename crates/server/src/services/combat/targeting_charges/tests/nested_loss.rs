//! Completion, missing-row batch policy and hull-order regressions for reload
//! reservations against destroyed nested contents. Only real physical external
//! sources enter a load's hints; deterministic held-lock barriers order every
//! race, never sleeps.
use super::*;
use super::super::completion::apply_loads;
use crate::dao::items::{creation, loading::{self, ItemQuery}, pending_reloads, transfers};
use crate::dao::items::parent_locks::tests::{backend_pid, wait_blocked};
use std::time::Duration;

struct LossFixture {
    f: crate::services::drones::tests::fixture::Fixture,
    module: i64,
    flag: i32,
    charge_type: i32,
}

async fn loss_fixture() -> LossFixture {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let (module_type, charge_type) = compatible_ammunition(&f.sde);
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let module = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, f.ship, flag).await.unwrap();
    LossFixture { f, module, flag, charge_type }
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn consumed_reservation_never_resurrects_charges() {
    let fx = loss_fixture().await;
    let (module, flag, charge_type, ship) = (fx.module, fx.flag, fx.charge_type, fx.f.ship);
    let db = fx.f.db.clone();
    let sde = fx.f.sde.clone();
    let owner = fx.f.owner;
    let f = fx.f;
    let result = crate::tests::catch_scenario(async {
        // Reservation whose weapon and source both live inside the doomed tree:
        // deleting the tree consumes the row, so a late completion finds
        // nothing and loads no charges.
        let doomed = pending_reloads::insert(&db, owner, ship, module, flag, charge_type, 3, FLAG_CARGO, ship).await.unwrap();
        let lost_sde = sde.clone();
        db.transaction(|tx, _: &mut Outbox| {
            let sde = lost_sde.clone();
            Box::pin(async move {
                let Some(_locks) = crate::services::ship::loss::lock_loss_items(tx, &sde, ship, None).await? else { return Ok::<(), CallError>(()); };
                crate::dao::items::ship::delete_lost(tx, ship, &[]).await.map_err(db_err)
            })
        }, |(), _| async move { Ok::<(), CallError>(()) }).await.unwrap();
        assert!(loading::load_items(&db, ItemQuery::Id(ship)).await.unwrap().is_empty());
        assert!(pending_reloads::take(&db, doomed).await.unwrap().is_none(), "the loss consumed the reservation row");
        let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let load = PendingLoad { owner: f.owner, reload_ids: vec![doomed], external_sources: vec![], external_facts: vec![], load: (module, flag, charge_type, 3, 0) };
        apply_loads(&f.db, &f.world, &notify, f.ship, vec![load]).await.unwrap();
        assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), None, "a consumed reservation must not resurrect charges");
        assert!(rx.try_recv().is_err());
        // Startup recovery must likewise leave no ammunition behind: the ship
        // is gone, so a surviving reservation is discarded, never refunded.
        recover_pending_reloads(&f.db, &f.sde).await.unwrap();
        assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), None);
    }).await;
    f.db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&f.owner]).await.unwrap();
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn external_batch_refuses_any_missing_reservation_while_internal_batch_skips() {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let (module_type, charge_type) = compatible_ammunition(&f.sde);
        let flag = infinity_core::consts::FLAG_FITTING_FIRST;
        let module = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, f.ship, flag).await.unwrap();
        let box_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
        let station = f.sde.map.stations().next().unwrap().station_id;
        let source = transfers::insert(&f.db, box_type, f.owner, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        f.db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&source]).await.unwrap();
        transfers::insert(&f.db, charge_type, f.owner, source, FLAG_CARGO, 20).await.unwrap();
        let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Physical-external batch: removing either row refuses the whole batch.
        for remove_physical in [true, false] {
            let physical = pending_reloads::insert(&f.db, f.owner, f.ship, module, flag, charge_type, 2, FLAG_CARGO, source).await.unwrap();
            let native = pending_reloads::insert(&f.db, f.owner, f.ship, module, flag, charge_type, 1, FLAG_CARGO, f.ship).await.unwrap();
            let fact = pending_reloads::for_ship(&f.db, f.ship).await.unwrap().into_iter().find(|row| row.reload_id == physical).unwrap();
            let before = pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
            if remove_physical { pending_reloads::take(&f.db, physical).await.unwrap(); }
            else { pending_reloads::take(&f.db, native).await.unwrap(); }
            let load = PendingLoad { owner: f.owner, reload_ids: vec![physical, native], external_sources: vec![source], external_facts: vec![fact], load: (module, flag, charge_type, 3, 0) };
            assert!(apply_loads(&f.db, &f.world, &notify, f.ship, vec![load]).await.is_err(), "a missing row in an external batch must refuse");
            assert!(pending_reloads::for_ship(&f.db, f.ship).await.unwrap() == if remove_physical { vec![before.into_iter().find(|row| row.reload_id == native).unwrap()] } else { vec![before.into_iter().find(|row| row.reload_id == physical).unwrap()] }, "the surviving reservation row must stay untouched");
            assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), None);
            assert!(rx.try_recv().is_err());
            for row in pending_reloads::for_ship(&f.db, f.ship).await.unwrap() { pending_reloads::take(&f.db, row.reload_id).await.unwrap(); }
        }
        // No physical external route: missing rows are skipped and the surviving
        // internal row still completes without resurrecting anything.
        let gone = pending_reloads::insert(&f.db, f.owner, f.ship, module, flag, charge_type, 2, FLAG_CARGO, f.ship).await.unwrap();
        let kept = pending_reloads::insert(&f.db, f.owner, f.ship, module, flag, charge_type, 4, FLAG_CARGO, f.ship).await.unwrap();
        pending_reloads::take(&f.db, gone).await.unwrap();
        let load = PendingLoad { owner: f.owner, reload_ids: vec![gone, kept], external_sources: vec![], external_facts: vec![], load: (module, flag, charge_type, 4, 0) };
        apply_loads(&f.db, &f.world, &notify, f.ship, vec![load]).await.unwrap();
        assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), Some((charge_type, 4)));
        assert!(pending_reloads::for_ship(&f.db, f.ship).await.unwrap().is_empty());
        crate::dao::items::combat::delete_charge(&f.db, f.ship, flag).await.unwrap();
        // Recovery must not resurrect charges for a ship whose reservation is
        // already consumed or gone; run it inside a fixture transaction so the
        // whole-database fingerprint stays intact.
        recover_pending_reloads(&f.db, &f.sde).await.unwrap();
        assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), None);
        f.db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&f.owner]).await.unwrap();
        f.db.execute("DELETE FROM game.items WHERE owner_id=$1 AND item_id != $2", &[&f.owner, &f.ship]).await.unwrap();
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn loss_preparation_takes_hulls_before_nonhulls_across_source_ancestry() {
    let f = crate::services::drones::tests::fixture::Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let ship_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
        let box_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
        let cargo_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
        // A doomed hull holds a stored hull and cargo rows; an external source
        // box lives under a second surviving hull so its real hull ancestors
        // enter the initial request set.
        let stored = creation::rookie_ship(&f.db, ship_type, f.owner, f.ship, FLAG_CARGO).await.unwrap();
        let older_cargo = transfers::insert(&f.db, cargo_type, f.owner, stored, FLAG_CARGO, 2).await.unwrap();
        assert!(older_cargo > 0 && stored > 0, "fixture rows exist");
        let other_hull = creation::positioned(&f.db, ship_type, f.owner, f.system, [1.0, 0.0, 0.0]).await.unwrap();
        let source_box = transfers::insert(&f.db, box_type, f.owner, other_hull, FLAG_CARGO, 1).await.unwrap();
        f.db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&source_box]).await.unwrap();
        let source_stack = transfers::insert(&f.db, cargo_type, f.owner, source_box, FLAG_CARGO, 4).await.unwrap();
        pending_reloads::insert(&f.db, f.owner, f.ship, f.ship, 1, cargo_type, 1, FLAG_CARGO, source_stack).await.unwrap();
        // Hold the newest hull's row to prove the fence acquires every hull
        // before touching a nonhull row anywhere in the request set.
        let config = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut holder_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let holder = holder_client.transaction().await.unwrap();
        holder.execute("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&f.ship]).await.unwrap();
        let pid = backend_pid(&holder).await;
        let sde = f.sde.clone();
        let db = f.db.clone();
        let ship = f.ship;
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_body = attempts.clone();
        let preparation = tokio::spawn(async move {
            db.transaction(|tx, _: &mut Outbox| {
                attempts_body.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let sde = sde.clone();
                Box::pin(async move { crate::services::ship::loss::lock_loss_items(tx, &sde, ship, None).await.map(|option| option.is_some()) })
            }, |value, _| async move { Ok::<bool, CallError>(value) }).await
        });
        tokio::task::yield_now().await;
        wait_blocked(&f.db, pid).await;
        // While the newer hull's row is held, the nonhull cargo and source stack
        // rows must still be unlocked: hulls precede nonhulls.
        let (probe, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        for id in [older_cargo, source_stack] {
            probe.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE NOWAIT", &[&id]).await.expect("nonhull row was locked before hull acquisition finished");
        }
        holder.rollback().await.unwrap();
        let held = tokio::time::timeout(Duration::from_secs(30), preparation).await.unwrap().unwrap().unwrap();
        assert!(held, "shared preparation must fence the complete request set");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1, "the stable crossing pair commits without a deadlock retry");
        assert_eq!(loading::load_items(&f.db, ItemQuery::Id(source_box)).await.unwrap().len(), 1);
        f.db.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&f.owner]).await.unwrap();
        f.db.execute("DELETE FROM game.items WHERE item_id=ANY($1) OR location_id=ANY($1)", &[&&[stored, other_hull, source_box][..]]).await.unwrap();
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
