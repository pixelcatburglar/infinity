//! Boundary loaded-hold proofs against a real system actor: claim covers
//! pending slots, competing holds refuse, cancellation rolls back, an armed
//! drop invalidates and one fresh reseed restores durable truth.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::{Fit, Key};
use crate::space::combat::loaded::LoadedSeed;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn held_loaded_settles_at_dock_commit_and_releases_on_refusal() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let (system, station_row) = (sde.map.systems().next().unwrap().solar_system_id,
        sde.map.stations().find(|row| row.solar_system_id.is_some() && row.constellation_id.is_some() && row.region_id.is_some()).unwrap());
    let destination = station::Destination { station_id: station_row.station_id, system_id: station_row.solar_system_id.unwrap(),
        constellation_id: station_row.constellation_id.unwrap(), region_id: station_row.region_id.unwrap() };
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let (account, owner, ship, module) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2) RETURNING character_id", &[&account, &system]).await?.get("character_id");
        let ship = creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
        tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &ship]).await?;
        let module: i64 = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($3,$2,$1,27,1,true) RETURNING item_id", &[&ship, &owner, &kind]).await?.get("item_id");
        tx.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1,27,$2,5)", &[&ship, &kind]).await?;
        Ok::<_, crate::db::Error>((account, owner, ship, module))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let result = crate::tests::catch_scenario(async {
        world.system(system).await.unwrap();
        world.remove_ball(system, ship).await;
        let row = FreeShip { item_id: ship, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = balls::piloted(&db, sde.clone(), &row, owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let empty_ship = -ship;
        let (loaded_membership, empty_membership) = world.with(system, { let sde = sde.clone(); move |s| {
            let fit = Fit::for_space_type(sde, kind, vec![]);
            let stats = ShipStats::from_fit(&fit, Key::Ship);
            s.install_condition(empty_ship, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
            (s.combat_membership_generation(ship).unwrap(), s.combat_membership_generation(empty_ship).unwrap())
        }}).await.unwrap();
        let mut held = quiesce_ship_loaded(&world, system, &[ship, empty_ship, i64::MAX]).await.unwrap();
        assert_eq!((held.claimed_membership(ship), held.claimed_membership(empty_ship), held.claimed_membership(i64::MAX)), (Some(loaded_membership), Some(empty_membership), None));
        world.with(system, move |s| { held.release_committed(s); s.actor_combat.remove(&empty_ship); }).await.unwrap();
        // Two payments leave a dirty absolute the boundary must settle.
        world.with(system, move |s| {
            let first = s.hold_loaded_use(ship, module).unwrap();
            assert_eq!(s.consume_loaded(first), Some((kind, 4)));
            let second = s.hold_loaded_use(ship, module).unwrap();
            assert_eq!(s.consume_loaded(second), Some((kind, 3)));
        }).await.unwrap();
        // A refused boundary keeps the business hold until the caller's guard drops.
        let held = quiesce_ship_loaded(&world, system, &[ship]).await.unwrap();
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        assert!(world.with(system, move |s| s.loaded_effective(ship, module)).await.unwrap().is_none(),
            "a cancelled boundary keeps the held slot until the guard drops");
        drop((transition, held));
        for _ in 0..200 {
            if world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten().is_some() { break; }
            tokio::task::yield_now().await;
        }
        assert_eq!(world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten().map(|v| (v.1, v.2)),
            Some((kind, 3)), "the released hold keeps the unpaid absolute");
        // A cancelled drain wait drops the claim receipt, releasing the held slot.
        let permit = world.with(system, move |s| s.hold_loaded_use(ship, module).unwrap()).await.unwrap();
        let task_world = world.clone();
        let claim = tokio::spawn(async move { quiesce_ship_loaded(&task_world, system, &[ship]).await });
        for _ in 0..200 {
            if world.with(system, move |s| s.actor_combat[&ship].loaded[&module].held).await.unwrap_or(false) { break; }
            tokio::task::yield_now().await;
        }
        assert!(world.with(system, move |s| s.actor_combat[&ship].loaded[&module].held).await.unwrap(),
            "the claim held the slot while its drain wait pended");
        claim.abort();
        let _ = claim.await;
        for _ in 0..200 {
            if world.with(system, move |s| !s.actor_combat[&ship].loaded[&module].held).await.unwrap_or(false) { break; }
            tokio::task::yield_now().await;
        }
        world.with(system, move |s| {
            assert!(s.actor_combat[&ship].loaded[&module].dirty, "the released claim restored the dirty absolute");
            s.release_loaded_use(permit);
        }).await.unwrap();
        // A competing hold refuses the whole claim; an armed guard that never
        // disposes invalidates its scope for a fresh reseed.
        let first = quiesce_ship_loaded(&world, system, &[ship]).await.unwrap();
        assert!(quiesce_ship_loaded(&world, system, &[ship]).await.is_err(), "a competing hold refuses the claim");
        drop(first);
        for _ in 0..200 {
            if world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten().is_some() { break; }
            tokio::task::yield_now().await;
        }
        let mut in_flight = quiesce_ship_loaded(&world, system, &[ship]).await.unwrap();
        in_flight.may_execute();
        drop(in_flight);
        let key = crate::dao::items::charges::LoadedSlotKey { ship, module, flag: 27 };
        for _ in 0..200 {
            if !world.with(system, move |s| s.unknown_loaded_keys()).await.unwrap().is_empty() { break; }
            tokio::task::yield_now().await;
        }
        assert_eq!(world.with(system, move |s| s.unknown_loaded_keys()).await.unwrap(), vec![key],
            "an undelivered armed hold stays held and unknown");
        crate::services::combat::loaded::reseed_held(&db, &world, system, &[key]).await.unwrap();
        assert_eq!(world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten().map(|v| (v.1, v.2)),
            Some((kind, 5)), "the fresh reseed restores the durable absolute");
        world.with(system, move |s| {
            let first = s.hold_loaded_use(ship, module).unwrap();
            assert_eq!(s.consume_loaded(first), Some((kind, 4)));
            let second = s.hold_loaded_use(ship, module).unwrap();
            assert_eq!(s.consume_loaded(second), Some((kind, 3)));
        }).await.unwrap();
        // A committed dock settles that dirty absolute inside its transaction.
        let held = quiesce_ship_loaded(&world, system, &[ship]).await.unwrap();
        let (admission, market, gate) = (world.admission(system).unwrap(), crate::services::market::lock().await,
            world.lock_condition_snapshot(system).await.unwrap());
        let mut transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        transition.held_loaded = Some(held);
        world.checkpoint_dock(db.clone(), owner, ship, system, destination, transition, market, gate, admission).await.unwrap();
        assert_eq!(crate::dao::items::charges::loaded(&db, ship, 27).await.unwrap().unwrap().quantity, 3);
        assert!(world.with(system, move |s| s.ball(ship).is_none()).await.unwrap());
    }).await;
    world.remove_ball(system, ship).await;
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&vec![ship, module]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&vec![ship, module]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[test]
fn stale_incarnation_receipt_is_inert_and_pending_is_not_outstanding() {
    crate::tests::init_config();
    let sde = crate::sde::types::tests::combat_fixture();
    let mut s = System::default();
    for ship in [7_i64, 8] {
        let fit = Fit::for_space_type(sde.clone(), 1, vec![]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        s.install_condition(ship, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    }
    let (m7, m8) = (s.combat_membership_generation(7).unwrap(), s.combat_membership_generation(8).unwrap());
    s.publish_loaded_absolute(7, m7, vec![
        LoadedSeed { module: 11, flag: 27, type_id: Some(9), quantity: 4, pending: false },
        LoadedSeed { module: 12, flag: 28, type_id: None, quantity: 0, pending: true }]);
    s.publish_loaded_absolute(8, m8, vec![
        LoadedSeed { module: 21, flag: 27, type_id: Some(9), quantity: 4, pending: false }]);
    s.record_loaded_period();
    assert!(!s.loaded_outstanding() && s.loaded_failed_periods == 0,
        "a durable pending reservation alone is not outstanding");
    let incarnation = s.actor_combat[&7].loaded[&11].incarnation;
    s.publish_loaded_absolute(7, m7, vec![
        LoadedSeed { module: 11, flag: 27, type_id: Some(9), quantity: 4, pending: false }]);
    let mut stale = HeldLoaded { world: std::sync::Weak::new(), system: 0, in_flight: false, resident: vec![(7, m7)],
        slots: vec![HeldSlot { ship: 7, membership: m7, module: 11, flag: 27, incarnation, dirty: true }] };
    assert!(stale.invalidate(&mut s).is_empty() && s.unknown_loaded_keys().is_empty(),
        "a stale-incarnation receipt invalidates nothing");
    let first = s.hold_loaded_use(7, 11).unwrap();
    assert_eq!(s.consume_loaded(first), Some((9, 3)));
    let second = s.hold_loaded_use(8, 21).unwrap();
    assert_eq!(s.consume_loaded(second), Some((9, 3)));
    s.record_loaded_period();
    let saved = s.capture_dirty_loaded(&[7]);
    s.clear_saved_loaded(&saved);
    assert!(!s.actor_combat[&7].loaded[&11].dirty && s.loaded_outstanding() && s.loaded_failed_periods > 0,
        "a neighbor's dirty keeps the counter past this ship's clean commit");
}
