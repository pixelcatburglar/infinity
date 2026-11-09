//! Manual ammunition operations against a foreign shared fleet hangar.
use super::*;
use infinity_core::consts::{CATEGORY_SHIP, FLAG_SHIP_HANGAR};
use infinity_core::consts::dogma::{ATTR_FLEET_HANGAR_CAPACITY, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY};
use crate::dao::items::{creation, inventory as item_inventory, ship_configuration::{self, Patch}};
use crate::services::drones::tests::fixture::Fixture;

async fn foreign_host(f: &Fixture, guest: i64, source: i64) -> i64 {
    let kind = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_FLEET_HANGAR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0))
        .expect("SDE ship with fleet hangar and maintenance bay").type_id;
    let host = creation::positioned(&f.db, kind, guest, f.system, [0.0; 3]).await.unwrap();
    let mut ball = f.world.ball(f.system, f.ship).await.unwrap();
    ball.id = host;
    ball.type_id = kind;
    ball.owner_id = guest;
    ball.char_id = None;
    ball.radius = f.sde.types.get(kind).and_then(|t| t.radius).unwrap_or(1.0) as f32;
    f.world.add_ship(f.system, ball).await.unwrap();
    let item = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert!(item_inventory::move_item(&f.db, &item, host, FLAG_FLEET_HANGAR, guest, false).await.unwrap());
    host
}

async fn settings(f: &Fixture, host: i64, guest: i64, fleet_hangar: bool, maintenance: bool) {
    let patch = Patch { fleet_hangar_corp: Some(fleet_hangar), maintenance_corp: Some(maintenance), ..Patch::default() };
    f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(crate::dao::items::ship::lock_reload_ship(tx, host).await?);
        assert!(ship_configuration::patch(tx, host, guest, &patch).await?);
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

async fn reload(f: &mut Fixture, module: i64, source: i64, location: i64) -> CallResult {
    let args = [Value::Int(f.ship), Value::Int(module), Value::list([Value::Int(source)]), Value::Int(location)];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "LoadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn foreign_fleet_hangar_requires_matching_current_corporation_grant() {
    let (mut f, guest, module, _, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(guest, sender);
    let (viewer, _) = f.add_pilot().await;
    let (excluded, _) = f.add_pilot().await;
    crate::services::tests::fixture::assign_corporation(&f.db, excluded, 0, 0).await.unwrap();
    let (viewer_tx, mut viewer_notices) = tokio::sync::mpsc::unbounded_channel();
    let (excluded_tx, mut excluded_notices) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(viewer, viewer_tx);
    f.world.register_pilot(excluded, excluded_tx);
    settings(&f, host, guest, false, true).await;
    let before = f.client.outbox.len();
    assert!(reload(&mut f, module, source, host).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap().is_empty());
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity, 100_000);
    settings(&f, host, guest, true, false).await;
    crate::services::tests::fixture::assign_corporation(&f.db, f.owner, 0, 0).await.unwrap();
    assert!(reload(&mut f, module, source, host).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity, 100_000);
    assert!(notices.try_recv().is_err());
    crate::services::tests::fixture::assign_corporation(&f.db, f.owner, 1, 0).await.unwrap();
    reload(&mut f, module, source, host).await.unwrap();
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].owner_id, f.owner);
    let remaining = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((remaining.owner_id, remaining.quantity), (guest, 100_000 - pending[0].quantity));
    let (event, args) = notices.try_recv().expect("source owner receives the quantity change");
    assert_eq!(event, "OnItemChange");
    assert_eq!(args[0], remaining.row());
    assert!(notices.try_recv().is_err(), "unrelated private rows reached the source owner");
    let (event, args) = viewer_notices.try_recv().expect("authorized bay viewer receives the quantity change");
    assert_eq!(event, "OnItemChange");
    assert_eq!(args[0], remaining.row());
    assert!(viewer_notices.try_recv().is_err());
    assert!(excluded_notices.try_recv().is_err(), "unauthorized pilot received a bay row");
    let amount = pending[0].quantity;
    let ids = vec![pending[0].reload_id];
    f.db.transaction(|tx, outbox: &mut Outbox| {
        let ids = ids.clone();
        let sde = f.sde.clone();
        let pending = pending.clone();
        Box::pin(async move {
            use crate::dao::items::parent_locks::{LockRequest, LockStrength};
            use crate::services::combat::reload_recovery::{RefundPreparation, return_pending};
            let mut refund = RefundPreparation::capture_pending(tx, &sde, &pending).await?;
            let mut requests: Vec<_> = pending.iter().flat_map(|row| [row.ship_id, row.source_location_id])
                .map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
            requests.extend_from_slice(refund.requests());
            let optional: Vec<_> = refund.candidate_ids().into_iter().filter(|id|
                !pending.iter().any(|row| row.ship_id == *id || row.source_location_id == *id)
                    && !refund.mandatory_ids().contains(id)).collect();
            let locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &optional).await?;
            return_pending(tx, &sde, outbox, &ids, true, &locks, &mut refund).await
        })
    }, |value, _: Outbox| async move { Ok(value) }).await.unwrap();
    let unchanged = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((unchanged.owner_id, unchanged.quantity), (guest, remaining.quantity));
    let refund = load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(f.ship, FLAG_CARGO, remaining.type_id, f.owner)).await.unwrap().pop().unwrap();
    assert_eq!(refund.quantity, amount);
    assert!(notices.try_recv().is_err(), "private refund reached the source owner");
    assert!(viewer_notices.try_recv().is_err(), "private refund reached the bay viewer");
    assert!(excluded_notices.try_recv().is_err());
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn maintenance_grant_does_not_reveal_foreign_stored_hull_cargo() {
    let (mut f, guest, module, _, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, false, true).await;
    let host_item = load_items(&f.db, &f.sde, ItemQuery::Id(host)).await.unwrap().pop().unwrap();
    let stored = creation::rookie_ship(&f.db, host_item.type_id, guest, host, FLAG_SHIP_HANGAR).await.unwrap();
    let item = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert!(item_inventory::move_item(&f.db, &item, stored, FLAG_CARGO, guest, false).await.unwrap());
    let before = f.client.outbox.len();
    assert!(reload(&mut f, module, source, stored).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(loaded(&f.db, f.ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap(), None);
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap().is_empty());
    let item = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((item.owner_id, item.location_id, item.quantity), (guest, stored, 100_000));
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn unload_into_foreign_box_rechecks_outer_hangar_and_ignores_claimed_owner() {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    let kind = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g))
        && t.capacity.is_some_and(|v| v > 0.0)).expect("cargo container type").type_id;
    let box_id = creation::rookie_ship(&f.db, kind, guest, host, FLAG_FLEET_HANGAR).await.unwrap();
    let module_type = load_items(&f.db, &f.sde, ItemQuery::Id(module)).await.unwrap()[0].type_id;
    let second_flag = infinity_core::consts::FLAG_FITTING_FIRST + 1;
    let second = creation::rookie_ship(&f.db, module_type, f.owner, f.ship, second_flag).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, f.ship, &[
        (module, infinity_core::consts::FLAG_FITTING_FIRST, charge_type, 2, 0),
        (second, second_flag, charge_type, 2, 0),
    ]).await.unwrap();
    let args = [Value::Int(f.ship), Value::list([Value::Int(module), Value::Int(second)]), Value::tuple([Value::Int(box_id), Value::Int(f.owner), Value::Int(i64::from(FLAG_HANGAR))])];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    settings(&f, host, guest, false, true).await;
    let before = f.client.outbox.len();
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(loaded(&f.db, f.ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap(), Some((charge_type, 2)));
    assert_eq!(loaded(&f.db, f.ship, second_flag).await.unwrap(), Some((charge_type, 2)));
    assert_eq!(f.client.outbox.len(), before);
    assert!(load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(box_id, FLAG_HANGAR, charge_type, f.owner)).await.unwrap().is_empty());
    settings(&f, host, guest, true, false).await;
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert_eq!(loaded(&f.db, f.ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap(), None);
    assert_eq!(loaded(&f.db, f.ship, second_flag).await.unwrap(), None);
    let output = load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(box_id, FLAG_HANGAR, charge_type, f.owner)).await.unwrap().pop().unwrap();
    assert_eq!(output.quantity, 4);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn foreign_hangar_reload_checks_sampled_range_and_visibility() {
    let (mut f, guest, module, _, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, true, false).await;
    let pilot = f.world.ball(f.system, f.ship).await.unwrap();
    let mut target = f.world.ball(f.system, host).await.unwrap();
    let limit = infinity_core::consts::MAX_CARGO_CONTAINER_TRANSFER_DISTANCE + f64::from(pilot.radius) + f64::from(target.radius);
    target.pos = [limit + 1.0, 0.0, 0.0];
    target.old_pos = target.pos;
    f.world.remove_ball(f.system, host).await;
    f.world.add_ship(f.system, target.clone()).await.unwrap();
    let before = f.client.outbox.len();
    assert!(reload(&mut f, module, source, host).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity, 100_000);
    target.pos = [limit, 0.0, 0.0];
    target.old_pos = target.pos;
    target.cloak = 1;
    f.world.remove_ball(f.system, host).await;
    f.world.add_ship(f.system, target.clone()).await.unwrap();
    assert!(reload(&mut f, module, source, host).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap().is_empty());
    target.cloak = 0;
    f.world.remove_ball(f.system, host).await;
    f.world.add_ship(f.system, target).await.unwrap();
    let other_system = f.sde.map.stations().find_map(|station| station.solar_system_id.filter(|id| *id != f.system)).unwrap();
    f.client.session.apply(vec![("solarsystemid", Value::Int(other_system))]);
    assert!(reload(&mut f, module, source, host).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity, 100_000);
    f.client.session.apply(vec![("solarsystemid", Value::Int(f.system))]);
    reload(&mut f, module, source, host).await.unwrap();
    assert_eq!(crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap().len(), 1);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn own_stored_hull_reload_still_requires_outer_maintenance_grant() {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    let hull = load_items(&f.db, &f.sde, ItemQuery::Id(host)).await.unwrap().pop().unwrap();
    let stored = creation::rookie_ship(&f.db, hull.type_id, f.owner, host, FLAG_SHIP_HANGAR).await.unwrap();
    let own = crate::dao::items::transfers::insert(&f.db, charge_type, f.owner, stored, FLAG_CARGO, 100_000).await.unwrap();
    settings(&f, host, guest, true, false).await;
    let before = f.client.outbox.len();
    assert!(reload(&mut f, module, own, stored).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap().is_empty());
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(own)).await.unwrap()[0].quantity, 100_000);
    settings(&f, host, guest, false, true).await;
    reload(&mut f, module, own, stored).await.unwrap();
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].owner_id, f.owner);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(own)).await.unwrap()[0].quantity, 100_000 - pending[0].quantity);
    f.cleanup().await;
}

async fn stale_parent_case(variant: u8) {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, true, false).await;
    let kind = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g))
        && t.capacity.is_some_and(|v| v > 0.0)).expect("cargo container type").type_id;
    let box_id = creation::rookie_ship(&f.db, kind, guest, host, FLAG_FLEET_HANGAR).await.unwrap();
    f.set(box_id, ATTR_CAPACITY, 1_000_000.0).await;
    let module_type = load_items(&f.db, &f.sde, ItemQuery::Id(module)).await.unwrap()[0].type_id;
    let second_flag = infinity_core::consts::FLAG_FITTING_FIRST + 1;
    let second = creation::rookie_ship(&f.db, module_type, f.owner, f.ship, second_flag).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, f.ship, &[
        (module, infinity_core::consts::FLAG_FITTING_FIRST, charge_type, 2, 0),
        (second, second_flag, charge_type, 2, 0),
    ]).await.unwrap();
    let old = load_items(&f.db, &f.sde, ItemQuery::Id(box_id)).await.unwrap().pop().unwrap();
    let config = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut connection, task) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { task.await.unwrap(); });
    let tx = connection.transaction().await.unwrap();
    let requests = [crate::dao::items::parent_locks::LockRequest { item_id: box_id, strength: crate::dao::items::parent_locks::LockStrength::Update }];
    let locks = crate::services::inventory::destination::lock_items(&tx, &f.sde, &requests, &[]).await.unwrap();
    assert!(locks.is_write_locked(box_id));
    drop(locks);
    let pid = crate::dao::items::parent_locks::tests::backend_pid(&tx).await;
    let args = [Value::Int(f.ship), Value::list([Value::Int(module), Value::Int(second)]), Value::tuple([Value::Int(box_id), Value::Int(guest), Value::Int(i64::from(FLAG_HANGAR))])];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    let db = f.db.clone();
    let ship = f.ship;
    let owner = f.owner;
    let before = f.client.outbox.len();
    let (result, ()) = tokio::join!(
        crate::services::dispatch(&mut f.client, &call),
        async move {
            crate::dao::items::parent_locks::tests::wait_blocked(&db, pid).await;
            let (location, flag, owner) = match variant {
                0 => (ship, FLAG_CARGO, guest),
                1 => (host, FLAG_FLEET_HANGAR, owner),
                _ => (host, FLAG_CARGO, guest),
            };
            assert!(item_inventory::move_item(&tx, &old, location, flag, owner, true).await.unwrap());
            tx.commit().await.unwrap();
        }
    );
    assert!(result.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(loaded(&f.db, f.ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap(), Some((charge_type, 2)));
    assert_eq!(loaded(&f.db, f.ship, second_flag).await.unwrap(), Some((charge_type, 2)));
    assert!(load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(box_id, FLAG_HANGAR, charge_type, f.owner)).await.unwrap().is_empty());
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn unload_rejects_parent_moved_while_waiting_for_guard() {
    for variant in 0..3 { stale_parent_case(variant).await; }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn unload_wrong_hold_type_preserves_magazine_and_pending() {
    let (mut f, _, module, charge_type, source) = shared_reload_fixture().await;
    let pending_id = crate::dao::items::pending_reloads::insert(
        &f.db, f.owner, f.ship, module, infinity_core::consts::FLAG_FITTING_FIRST,
        charge_type, 1, FLAG_FLEET_HANGAR, f.ship,
    ).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, f.ship, &[(module, infinity_core::consts::FLAG_FITTING_FIRST, charge_type, 2, 0)]).await.unwrap();
    f.set(f.ship, infinity_core::consts::dogma::ATTR_SPECIAL_FUEL_BAY_CAPACITY, 1_000_000.0).await;
    let args = [Value::Int(f.ship), Value::Int(module), Value::tuple([
        Value::Int(f.ship), Value::Int(f.owner), Value::Int(i64::from(infinity_core::consts::FLAG_SPECIALIZED_FUEL_BAY)),
    ])];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    let before = f.client.outbox.len();
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(loaded(&f.db, f.ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap(), Some((charge_type, 2)));
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].reload_id, pending_id);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity, 100_000);
    assert!(load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(f.ship, infinity_core::consts::FLAG_SPECIALIZED_FUEL_BAY, charge_type, f.owner)).await.unwrap().is_empty());
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn foreign_fleet_hangar_rechecks_live_fleet_membership() {
    let (mut f, guest, module, _, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    let patch = Patch { fleet_hangar_fleet: Some(true), fleet_hangar_corp: Some(false), ..Patch::default() };
    f.db.transaction(|tx, _: &mut ()| Box::pin(async move {
        assert!(crate::dao::items::ship::lock_reload_ship(tx, host).await?);
        assert!(ship_configuration::patch(tx, host, guest, &patch).await?);
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    assert!(reload(&mut f, module, source, host).await.is_err());
    let guest_ship = f.world.with(f.system, move |s| s.pilot_ball(guest).map(|ball| ball.id)).await.unwrap().unwrap();
    let fleet = crate::services::drones::tests::resources::fleet::form_fleet(&mut f, guest, guest_ship).await.unwrap();
    reload(&mut f, module, source, host).await.unwrap();
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
    assert_eq!(pending.len(), 1);
    let remaining = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity;
    let params = Value::Int(fleet);
    let kwargs = Value::None;
    let args = [Value::Int(guest)];
    let kick = Call { service: "fleetObjectHandler", method: "KickMember", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&params) };
    crate::services::dispatch(&mut f.client, &kick).await.unwrap();
    assert!(!crate::services::fleets::same_fleet(f.owner, guest));
    let before = f.client.outbox.len();
    assert!(reload(&mut f, module, source, host).await.is_err());
    assert_eq!(f.client.outbox.len(), before);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap()[0].quantity, remaining);
    let after = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].reload_id, pending[0].reload_id);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn taking_source_box_settles_actor_reservation_before_custody_transfer() {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, true, false).await;
    let kind = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g))
        && t.capacity.is_some_and(|v| v > 0.0)).expect("cargo container type").type_id;
    let box_id = creation::rookie_ship(&f.db, kind, guest, host, FLAG_FLEET_HANGAR).await.unwrap();
    let old = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert!(item_inventory::move_item(&f.db, &old, box_id, FLAG_HANGAR, guest, false).await.unwrap());
    reload(&mut f, module, source, box_id).await.unwrap();
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].owner_id, f.owner);
    let amount = pending[0].quantity;
    let remaining = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((remaining.owner_id, remaining.quantity), (guest, 100_000 - amount));
    let (guest_tx, mut guest_notices) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(guest, guest_tx);
    let bind = Value::tuple([Value::Int(f.ship)]);
    let args = [Value::Int(box_id), Value::Int(host)];
    let kwargs = Value::Dict(vec![(Value::str("flag"), Value::Int(i64::from(FLAG_CARGO)))]);
    let call = Call { service: "invInventory", method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert!(crate::dao::items::pending_reloads::for_ship(&f.db, f.ship).await.unwrap().is_empty());
    let after = load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((after.owner_id, after.location_id, after.quantity), (f.owner, box_id, remaining.quantity));
    let moved = load_items(&f.db, &f.sde, ItemQuery::Id(box_id)).await.unwrap().pop().unwrap();
    assert_eq!((moved.owner_id, moved.location_id), (f.owner, f.ship));
    let refund = load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(f.ship, FLAG_CARGO, charge_type, f.owner)).await.unwrap().pop().unwrap();
    assert_eq!(refund.quantity, amount);
    let refunded = load_items(&f.db, &f.sde, ItemQuery::Id(refund.item_id)).await.unwrap().pop().unwrap();
    while let Ok((event, args)) = guest_notices.try_recv() {
        assert!(event != "OnItemChange" || args.first() != Some(&refunded.row()), "private refund reached the former box owner");
    }
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn unload_shared_bay_notifies_actual_atomic_stack_receipt() {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, true, false).await;
    f.set(host, ATTR_FLEET_HANGAR_CAPACITY, 1_000_000_000.0).await;
    let (viewer, _) = f.add_pilot().await;
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(viewer, sender);
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    for quantities in [vec![3, 7], vec![0], vec![]] {
        let mut before = std::collections::BTreeMap::new();
        for quantity in quantities {
            let id = crate::dao::items::transfers::insert(&f.db, charge_type, f.owner, host, FLAG_FLEET_HANGAR, quantity).await.unwrap();
            before.insert(id, quantity);
        }
        crate::dao::items::combat::apply_loads(&f.db, f.ship, &[(module, flag, charge_type, 2, 0)]).await.unwrap();
        let args = [Value::Int(f.ship), Value::Int(module), Value::tuple([
            Value::Int(host), Value::Int(guest), Value::Int(i64::from(FLAG_FLEET_HANGAR)),
        ])];
        let kwargs = Value::None;
        let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dispatch(&mut f.client, &call).await.unwrap();
        assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), None);
        let rows: Vec<_> = load_items(&f.db, &f.sde, ItemQuery::Location(host)).await.unwrap().into_iter()
            .filter(|item| item.owner_id == f.owner && item.flag_id == FLAG_FLEET_HANGAR && item.type_id == charge_type).collect();
        assert_eq!(rows.len(), before.len().max(1));
        let changed: Vec<_> = rows.iter().filter(|item| before.get(&item.item_id).copied() != Some(item.quantity)).collect();
        assert_eq!(changed.len(), 1);
        let actual = changed[0];
        assert!(!actual.singleton);
        assert_eq!((actual.owner_id, actual.location_id, actual.flag_id), (f.owner, host, FLAG_FLEET_HANGAR));
        let change = match before.get(&actual.item_id) {
            Some(old) => {
                assert_eq!(actual.quantity, old + 2);
                (IX_STACK_SIZE, Value::Int(i64::from(*old)))
            }
            None => {
                assert!(before.is_empty());
                assert_eq!(actual.quantity, 2);
                (IX_LOCATION_ID, Value::Int(f.ship))
            }
        };
        for item in &rows {
            if item.item_id != actual.item_id { assert_eq!(Some(&item.quantity), before.get(&item.item_id)); }
        }
        let (event, payload) = notices.try_recv().expect("authorized non-actor viewer receives the deposit");
        assert_eq!(event, "OnItemChange");
        assert_eq!(payload, vec![actual.row(), Value::Dict(vec![(Value::Int(change.0), change.1)]), Value::None]);
        assert!(notices.try_recv().is_err(), "unexpected additional viewer notification");
        for item in rows { item_inventory::delete(&f.db, item.item_id).await.unwrap(); }
    }
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn selected_box_unload_refuses_without_room_then_deposits() {
    let (mut f, guest, module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, true, false).await;
    let unit = crate::services::item_volume::item_volume(&f.sde, charge_type, false, 1, None).unwrap();
    assert!(unit > 0.0);
    let kind = f.sde.types.iter().find(|t| {
        if t.published != Some(true) || !t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g)) { return false; }
        let capacity = crate::services::inventory::hold_storage::static_capacity(&f.sde, t.type_id).unwrap();
        capacity.is_finite() && capacity >= 2.0 * unit && capacity / unit < f64::from(i32::MAX)
    }).expect("ordinary container with room for ammunition").type_id;
    let capacity = crate::services::inventory::hold_storage::static_capacity(&f.sde, kind).unwrap();
    let box_id = creation::rookie_ship(&f.db, kind, guest, host, FLAG_FLEET_HANGAR).await.unwrap();
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    crate::dao::items::combat::apply_loads(&f.db, f.ship, &[(module, flag, charge_type, 2, 0)]).await.unwrap();
    let quantity = (capacity / unit).floor() as i64;
    let filler = crate::dao::items::transfers::insert(&f.db, charge_type, f.owner, box_id, FLAG_HANGAR, quantity).await.unwrap();
    let args = [Value::Int(f.ship), Value::Int(module), Value::tuple([
        Value::Int(box_id), Value::Int(guest), Value::Int(i64::from(FLAG_HANGAR)),
    ])];
    let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    let before = f.client.outbox.len();
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), Some((charge_type, 2)));
    assert_eq!(f.client.outbox.len(), before);
    let unchanged = load_items(&f.db, &f.sde, ItemQuery::Id(filler)).await.unwrap().pop().unwrap();
    assert_eq!(unchanged.quantity, quantity);
    if quantity > 1 {
        f.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&filler, &(quantity - 1)]).await.unwrap();
        assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
        assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), Some((charge_type, 2)));
        assert_eq!(f.client.outbox.len(), before);
        assert_eq!(load_items(&f.db, &f.sde, ItemQuery::Id(filler)).await.unwrap()[0].quantity, quantity - 1);
    }
    item_inventory::delete(&f.db, filler).await.unwrap();
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert_eq!(loaded(&f.db, f.ship, flag).await.unwrap(), None);
    let deposited = load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(box_id, FLAG_HANGAR, charge_type, f.owner)).await.unwrap().pop().unwrap();
    let deposited = load_items(&f.db, &f.sde, ItemQuery::Id(deposited.item_id)).await.unwrap().pop().unwrap();
    assert_eq!((deposited.quantity, deposited.owner_id, deposited.location_id, deposited.flag_id), (2, f.owner, box_id, FLAG_HANGAR));
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn selected_unload_locks_box_before_module() {
    use std::time::Duration;
    let (mut f, guest, original_module, charge_type, source) = shared_reload_fixture().await;
    let host = foreign_host(&f, guest, source).await;
    settings(&f, host, guest, true, false).await;
    let unit = crate::services::item_volume::item_volume(&f.sde, charge_type, false, 1, None).unwrap();
    let kind = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g))
        && crate::services::inventory::hold_storage::static_capacity(&f.sde, t.type_id).is_ok_and(|c| c >= 2.0 * unit))
        .expect("ordinary container with room for ammunition").type_id;
    let box_id = creation::rookie_ship(&f.db, kind, guest, host, FLAG_FLEET_HANGAR).await.unwrap();
    let flag = infinity_core::consts::FLAG_FITTING_FIRST + 1;
    let module_type = load_items(&f.db, &f.sde, ItemQuery::Id(original_module)).await.unwrap()[0].type_id;
    let module = creation::rookie_ship(&f.db, module_type, f.owner, f.ship, flag).await.unwrap();
    crate::dao::items::combat::apply_loads(&f.db, f.ship, &[(module, flag, charge_type, 2, 0)]).await.unwrap();
    let config = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut holder_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let holder = holder_client.transaction().await.unwrap();
    holder.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&module]).await.unwrap();
    let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
    let db = f.db.clone();
    let ship = f.ship;
    let operation = tokio::spawn(async move {
        let args = [Value::Int(ship), Value::Int(module), Value::tuple([
            Value::Int(box_id), Value::Int(guest), Value::Int(i64::from(FLAG_HANGAR)),
        ])];
        let kwargs = Value::None;
        let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        let result = crate::services::dispatch(&mut f.client, &call).await;
        (f, result)
    });
    tokio::time::timeout(Duration::from_secs(30), crate::dao::items::parent_locks::tests::wait_blocked(&db, pid)).await.unwrap();
    let (probe_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let probe = probe_client.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR SHARE NOWAIT", &[&box_id]).await;
    holder.rollback().await.unwrap();
    let (f, result) = tokio::time::timeout(Duration::from_secs(30), operation).await.unwrap().unwrap();
    assert!(result.is_ok(), "selected unload failed: {result:?}");
    assert_eq!(loaded(&f.db, ship, flag).await.unwrap(), None);
    assert_eq!(load_items(&f.db, &f.sde, ItemQuery::LocationFlagTypeOwnerStack(box_id, FLAG_HANGAR, charge_type, f.owner)).await.unwrap()[0].quantity, 2);
    assert!(matches!(probe, Err(error) if error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)));
    f.cleanup().await;
}
