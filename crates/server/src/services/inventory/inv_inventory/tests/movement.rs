//! Real bound inventory mutation regressions for shared ship bays.
//! Uses the shared database and World fixture; client source is invCache.py.
//! Stored-ship lifecycle and destruction are covered by their owning services.
use super::*;
use super::fleet::fleet_rpc;
use crate::dao::items::{creation, inventory as item_inventory, ship_configuration::{self, Patch}, transfers};
use infinity_core::consts::dogma::{ATTR_CAPACITY, ATTR_FLEET_HANGAR_CAPACITY, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE and external flock"]
async fn guest_add_multiadd_recheck_independent_corp_and_fleet_switches() {
    let mut f = setup().await;
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    f.client.notify_tx = sender;
    assert!(crate::dao::items::dogma::set_override(&f.db, f.host, ATTR_FLEET_HANGAR_CAPACITY, 1_000_000_000_000.0).await.unwrap());
    assert!(crate::dao::items::dogma::set_override(&f.db, f.pilot, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY, 1_000_000_000_000.0).await.unwrap());
    assert!(crate::dao::items::dogma::set_override(&f.db, f.pilot, ATTR_CAPACITY, 1_000_000_000_000.0).await.unwrap());
    let host_bind = Value::tuple([Value::Int(f.host)]);
    let pilot_bind = Value::tuple([Value::Int(f.pilot)]);
    let fleet_flag = Value::Dict(vec![(Value::str("flag"), Value::Int(i64::from(FLAG_FLEET_HANGAR)))]);
    let maintenance_flag = Value::Dict(vec![(Value::str("flag"), Value::Int(i64::from(FLAG_SHIP_HANGAR)))]);
    let empty = Value::Dict(vec![]);
    let args = [Value::Int(f.own_cargo), Value::Int(f.pilot)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &fleet_flag, bound: true, bind_params: Some(&host_bind) };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let deposited = load_items(&f.db, &f.client.sde, ItemQuery::Id(f.own_cargo)).await.unwrap().pop().unwrap();
    assert_eq!((deposited.owner_id, deposited.location_id, deposited.flag_id), (f.actor.character, f.host, FLAG_FLEET_HANGAR));

    // The first item commits; the second is in a foreign hull's private cargo.
    let args = [Value::list([Value::Int(f.theirs), Value::Int(f.private)])];
    let call = Call { service: super::super::super::INVENTORY, method: "MultiAdd", args: &args, kwargs: &empty, bound: true, bind_params: Some(&pilot_bind) };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    let taken = load_items(&f.db, &f.client.sde, ItemQuery::Id(f.theirs)).await.unwrap().pop().unwrap();
    let private = load_items(&f.db, &f.client.sde, ItemQuery::Id(f.private)).await.unwrap().pop().unwrap();
    assert_eq!((taken.owner_id, taken.location_id), (f.actor.character, f.pilot));
    assert_eq!((private.owner_id, private.location_id, private.flag_id), (f.owner.character, f.stored, FLAG_CARGO));

    let blocked = transfers::insert(&f.db, f.commodity, f.actor.character, f.pilot, FLAG_CARGO, 1).await.unwrap();
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(false), maintenance_corp: Some(true), ..Patch::default() }).await.unwrap();
    let args = [Value::Int(blocked), Value::Int(f.pilot)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &fleet_flag, bound: true, bind_params: Some(&host_bind) };
    while notices.try_recv().is_ok() {}
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert!(notices.try_recv().is_err(), "denied mutation must not emit notices");
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(blocked)).await.unwrap().pop().unwrap().location_id, f.pilot);
    let args = [Value::Int(f.stored), Value::Int(f.host)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &maintenance_flag, bound: true, bind_params: Some(&pilot_bind) };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let rows = load_items(&f.db, &f.client.sde, ItemQuery::Ids(&[f.stored, f.private])).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.owner_id == f.actor.character));
    assert_eq!(rows.iter().find(|row| row.item_id == f.private).unwrap().location_id, f.stored);

    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(true), maintenance_corp: Some(false), ..Patch::default() }).await.unwrap();
    fixture::assign_corporation(&f.db, f.actor.character, 0, 0).await.unwrap();
    let args = [Value::Int(blocked), Value::Int(f.pilot)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &fleet_flag, bound: true, bind_params: Some(&host_bind) };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    fixture::assign_corporation(&f.db, f.actor.character, f.corp, 0).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&f.db, f.host, ATTR_FLEET_HANGAR_CAPACITY, 0.0).await.unwrap());
    while notices.try_recv().is_ok() {}
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert!(notices.try_recv().is_err(), "full bay refusal must not emit notices");
    let unchanged = load_items(&f.db, &f.client.sde, ItemQuery::Id(blocked)).await.unwrap().pop().unwrap();
    assert_eq!((unchanged.owner_id, unchanged.location_id, unchanged.quantity), (f.actor.character, f.pilot, 1));
    assert!(crate::dao::items::dogma::set_override(&f.db, f.host, ATTR_FLEET_HANGAR_CAPACITY, 1_000_000_000_000.0).await.unwrap());
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(false), fleet_hangar_fleet: Some(true), maintenance_fleet: Some(false), ..Patch::default() }).await.unwrap();

    let (mut leader, _peer) = crate::services::tests::client(f.db.clone(), f.owner.character).await;
    leader.world = f.client.world.clone();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    f.client.world.register_pilot(f.actor.character, tx);
    f.client.world.set_online_session(f.actor.character, Some(infinity_core::session::OnlineSession {
        character_id: f.actor.character, user_id: None, session_id: f.client.session.sid,
        ship_id: Some(f.pilot), station_id: None, system_id: Some(f.system), corporation_id: Some(f.corp),
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    fleet_rpc(&mut leader, None, "CreateFleet", &[]).await.unwrap();
    let fleet = leader.bound.values().find_map(|(service, param)| (service == "fleetObjectHandler").then(|| param.as_int()).flatten()).unwrap();
    fleet_rpc(&mut leader, Some(fleet), "Init", &[Value::Int(i64::from(f.hull_type)), Value::None, Value::None]).await.unwrap();
    fleet_rpc(&mut leader, Some(fleet), "Invite", &[Value::Int(f.actor.character), Value::None, Value::None, Value::None]).await.unwrap();
    fleet_rpc(&mut f.client, Some(fleet), "AcceptInvite", &[Value::Int(i64::from(f.hull_type))]).await.unwrap();
    let args = [Value::Int(blocked), Value::Int(f.pilot)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &fleet_flag, bound: true, bind_params: Some(&host_bind) };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let row = load_items(&f.db, &f.client.sde, ItemQuery::Id(blocked)).await.unwrap().pop().unwrap();
    assert_eq!((row.owner_id, row.location_id), (f.actor.character, f.host));

    let stored = creation::rookie_ship(&f.db, f.hull_type, f.owner.character, f.host, FLAG_SHIP_HANGAR).await.unwrap();
    let args = [Value::Int(stored), Value::Int(f.host)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &maintenance_flag, bound: true, bind_params: Some(&pilot_bind) };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_fleet: Some(false), maintenance_fleet: Some(true), ..Patch::default() }).await.unwrap();
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let row = load_items(&f.db, &f.client.sde, ItemQuery::Id(stored)).await.unwrap().pop().unwrap();
    assert_eq!((row.owner_id, row.location_id, row.flag_id), (f.actor.character, f.pilot, FLAG_SHIP_HANGAR));

    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_fleet: Some(true), ..Patch::default() }).await.unwrap();
    fleet_rpc(&mut leader, Some(fleet), "KickMember", &[Value::Int(f.actor.character)]).await.unwrap();
    let after_kick = transfers::insert(&f.db, f.commodity, f.actor.character, f.pilot, FLAG_CARGO, 1).await.unwrap();
    let args = [Value::Int(after_kick), Value::Int(f.pilot)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &fleet_flag, bound: true, bind_params: Some(&host_bind) };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(after_kick)).await.unwrap().pop().unwrap().location_id, f.pilot);
    fleet_rpc(&mut leader, Some(fleet), "DisbandFleet", &[]).await.unwrap();
    f.client.world.unregister_pilot_generation(f.actor.character, None);
    for id in [blocked, stored, after_kick] { item_inventory::delete(&f.db, id).await.unwrap(); }
    cleanup(f).await;
}
