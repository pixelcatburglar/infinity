//! Global inventory metadata dispatch regressions with real shared-bay state.
//! Mirrors invbroker and expertSystemMgr clients; uses the shared World and SDE fixture.
//! Unrelated crystal mutation and expert lifecycle behavior are covered separately.
use super::*;
use infinity_core::consts::dogma::{ATTR_CAPACITY, ATTR_CRYSTAL_DAMAGE_ENABLED, ATTR_HULL_DAMAGE};
use crate::dao::items::{creation, inventory as item_inventory, ship_configuration::{self, Patch}, transfers};
use crate::services::fit;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE and external flock"]
async fn crystal_visibility_and_label_custody_recheck_shared_and_private_ancestry() {
    let mut f = setup().await;
    f.client.session.apply(vec![("solarsystemid2", Value::Int(f.system))]);
    let crystal = f.client.sde.types.iter().find(|ty| {
        let enabled = fit::base_attribute(Some(ty), f.client.sde.dogma.attribute(ATTR_CRYSTAL_DAMAGE_ENABLED), ATTR_CRYSTAL_DAMAGE_ENABLED);
        let damage = fit::base_attribute(Some(ty), f.client.sde.dogma.attribute(ATTR_HULL_DAMAGE), ATTR_HULL_DAMAGE);
        enabled.is_finite() && enabled > 0.0 && damage.is_finite() && (0.0..=1.0).contains(&damage)
    }).expect("SDE contains a damageable crystal").type_id;
    let can = f.client.sde.types.iter().find(|ty| ty.group_id.is_some_and(|group| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&group))).unwrap().type_id;
    let shared = transfers::insert(&f.db, crystal, f.owner.character, f.host, FLAG_FLEET_HANGAR, 1).await.unwrap();
    let private = transfers::insert(&f.db, crystal, f.actor.character, f.stored, FLAG_CARGO, 1).await.unwrap();
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { maintenance_corp: Some(true), ..Patch::default() }).await.unwrap();
    let ids = [Value::list([Value::Int(shared), Value::Int(private)])];
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "invbroker", method: "GetDamageForCrystals", args: &ids, kwargs: &kwargs, bound: true, bind_params: None };
    let value = crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let Value::Dict(rows) = value else { panic!("crystal damage must be a dictionary") };
    assert!(rows.iter().any(|(key, _)| key.as_int() == Some(shared)));
    assert!(!rows.iter().any(|(key, _)| key.as_int() == Some(private)));
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(false), ..Patch::default() }).await.unwrap();
    let value = crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert!(matches!(value, Value::Dict(rows) if rows.is_empty()));
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(true), ..Patch::default() }).await.unwrap();

    let guest_can = creation::rookie_ship(&f.db, can, f.owner.character, f.host, FLAG_FLEET_HANGAR).await.unwrap();
    let private_can = creation::rookie_ship(&f.db, can, f.actor.character, f.stored, FLAG_CARGO).await.unwrap();
    let standalone = creation::rookie_ship(&f.db, can, f.actor.character, f.station, FLAG_HANGAR).await.unwrap();
    let label = Value::unicode("示例");
    let args = [Value::Int(guest_can), label.clone()];
    let call = Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    let args = [Value::Int(private_can), label.clone()];
    let call = Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    let args = [Value::Int(standalone), label.clone()];
    let call = Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let args = [Value::Int(f.pilot), label.clone()];
    let call = Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&f.db, f.pilot, ATTR_CAPACITY, 1_000_000_000_000.0).await.unwrap());
    let pilot_bind = Value::tuple([Value::Int(f.pilot)]);
    let args = [Value::Int(guest_can), Value::Int(f.host)];
    let call = Call { service: super::super::super::INVENTORY, method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&pilot_bind) };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(guest_can)).await.unwrap().pop().unwrap().owner_id, f.actor.character);
    let args = [Value::Int(guest_can), label];
    let call = Call { service: "invbroker", method: "SetLabel", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    for id in [shared, private, guest_can, private_can, standalone] { item_inventory::delete(&f.db, id).await.unwrap(); }
    cleanup(f).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE and external flock"]
async fn shared_expert_consume_notifies_owner_but_keeps_installation_private() {
    let mut f = setup().await;
    f.client.session.apply(vec![("solarsystemid2", Value::Int(f.system))]);
    let expert = f.client.sde.expert_systems.iter().find(|row| !row.hidden && !row.retired).expect("SDE contains an available expert system").type_id;
    let item = transfers::insert(&f.db, expert, f.owner.character, f.host, FLAG_FLEET_HANGAR, 2).await.unwrap();
    let viewer = fixture::create(&f.db).await.unwrap();
    fixture::assign_corporation(&f.db, viewer.character, f.corp, 0).await.unwrap();
    let viewer_ship = creation::rookie_ship(&f.db, f.hull_type, viewer.character, f.system, FLAG_HANGAR).await.unwrap();
    let viewer_ball = ball(f.hull_type, f.corp, viewer_ship, viewer.character, 2600.0, Some(viewer.character));
    let owner_ship = creation::rookie_ship(&f.db, f.hull_type, f.owner.character, f.system, FLAG_HANGAR).await.unwrap();
    let outsider_ship = creation::rookie_ship(&f.db, f.hull_type, f.outsider.character, f.system, FLAG_HANGAR).await.unwrap();
    let owner_ball = ball(f.hull_type, f.corp, owner_ship, f.owner.character, 2600.0, Some(f.owner.character));
    let outsider_ball = ball(f.hull_type, 0, outsider_ship, f.outsider.character, 2600.0, Some(f.outsider.character));
    f.client.world.with(f.system, move |space| { space.insert(owner_ball); space.insert(outsider_ball); space.insert(viewer_ball); }).await.unwrap();
    let (actor_tx, mut actor_rx) = tokio::sync::mpsc::unbounded_channel();
    let (owner_tx, mut owner_rx) = tokio::sync::mpsc::unbounded_channel();
    let (outsider_tx, mut outsider_rx) = tokio::sync::mpsc::unbounded_channel();
    let (viewer_tx, mut viewer_rx) = tokio::sync::mpsc::unbounded_channel();
    f.client.notify_tx = actor_tx;
    f.client.world.register_pilot(f.owner.character, owner_tx);
    f.client.world.register_pilot(f.outsider.character, outsider_tx);
    f.client.world.register_pilot(viewer.character, viewer_tx);
    let hidden = transfers::insert(&f.db, expert, f.actor.character, f.stored, FLAG_CARGO, 2).await.unwrap();
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { maintenance_corp: Some(true), fleet_hangar_corp: Some(false), ..Patch::default() }).await.unwrap();
    let kwargs = Value::None;
    let args = [Value::Int(hidden)];
    let call = Call { service: "expertSystemMgr", method: "ConsumeExpertSystem", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    let args = [Value::Int(item)];
    let call = Call { service: "expertSystemMgr", method: "ConsumeExpertSystem", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(true), ..Patch::default() }).await.unwrap();
    let far = ball(f.hull_type, f.corp, f.host, f.owner.character, 1_000_000.0, None);
    f.client.world.with(f.system, move |space| { space.insert(far); }).await.unwrap();
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    let near = ball(f.hull_type, f.corp, f.host, f.owner.character, 2600.0, None);
    f.client.world.with(f.system, move |space| { space.insert(near); }).await.unwrap();
    for id in [item, hidden] {
        assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap().quantity, 2);
    }
    assert!(crate::dao::skills::expert_systems::all(&f.db, f.actor.character).await.unwrap().is_empty());
    assert!(actor_rx.try_recv().is_err());
    assert!(owner_rx.try_recv().is_err());
    assert!(viewer_rx.try_recv().is_err());
    assert!(outsider_rx.try_recv().is_err());
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch {
        fleet_hangar_corp: Some(false), fleet_hangar_fleet: Some(true), ..Patch::default()
    }).await.unwrap();
    let (mut leader, _leader_peer) = crate::services::tests::client(f.db.clone(), f.owner.character).await;
    leader.world = f.client.world.clone();
    let (fleet_tx, _fleet_rx) = tokio::sync::mpsc::unbounded_channel();
    f.client.world.register_pilot(f.actor.character, fleet_tx);
    f.client.world.set_online_session(f.actor.character, Some(infinity_core::session::OnlineSession {
        character_id: f.actor.character, user_id: None, session_id: f.client.session.sid,
        ship_id: Some(f.pilot), station_id: None, system_id: Some(f.system), corporation_id: Some(f.corp),
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    super::fleet::fleet_rpc(&mut leader, None, "CreateFleet", &[]).await.unwrap();
    let fleet = leader.bound.values().find_map(|(service, param)| (service == "fleetObjectHandler").then(|| param.as_int()).flatten()).unwrap();
    super::fleet::fleet_rpc(&mut leader, Some(fleet), "Init", &[Value::Int(i64::from(f.hull_type)), Value::None, Value::None]).await.unwrap();
    super::fleet::fleet_rpc(&mut leader, Some(fleet), "Invite", &[Value::Int(f.actor.character), Value::None, Value::None, Value::None]).await.unwrap();
    super::fleet::fleet_rpc(&mut f.client, Some(fleet), "AcceptInvite", &[Value::Int(i64::from(f.hull_type))]).await.unwrap();
    assert!(crate::services::fleets::same_fleet(f.actor.character, f.owner.character));
    super::fleet::fleet_rpc(&mut leader, Some(fleet), "KickMember", &[Value::Int(f.actor.character)]).await.unwrap();
    assert!(!crate::services::fleets::same_fleet(f.actor.character, f.owner.character));
    while actor_rx.try_recv().is_ok() {}
    while owner_rx.try_recv().is_ok() {}
    while viewer_rx.try_recv().is_ok() {}
    while outsider_rx.try_recv().is_ok() {}
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(item)).await.unwrap().pop().unwrap().quantity, 2);
    assert!(crate::dao::skills::expert_systems::all(&f.db, f.actor.character).await.unwrap().is_empty());
    assert!(actor_rx.try_recv().is_err() && owner_rx.try_recv().is_err() && viewer_rx.try_recv().is_err() && outsider_rx.try_recv().is_err());
    super::fleet::fleet_rpc(&mut leader, Some(fleet), "DisbandFleet", &[]).await.unwrap();
    f.client.world.unregister_pilot_generation(f.actor.character, None);
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch { fleet_hangar_corp: Some(true), ..Patch::default() }).await.unwrap();
    let invalid = transfers::insert(&f.db, f.commodity, f.owner.character, f.host, FLAG_FLEET_HANGAR, 2).await.unwrap();
    let invalid_args = [Value::Int(invalid)];
    let invalid_call = Call { service: "expertSystemMgr", method: "ConsumeExpertSystem", args: &invalid_args, kwargs: &kwargs, bound: false, bind_params: None };
    while actor_rx.try_recv().is_ok() {}
    while owner_rx.try_recv().is_ok() {}
    while viewer_rx.try_recv().is_ok() {}
    assert!(crate::services::dispatch(&mut f.client, &invalid_call).await.is_err());
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(invalid)).await.unwrap().pop().unwrap().quantity, 2);
    assert!(crate::dao::skills::expert_systems::all(&f.db, f.actor.character).await.unwrap().is_empty());
    assert!(actor_rx.try_recv().is_err() && owner_rx.try_recv().is_err() && viewer_rx.try_recv().is_err() && outsider_rx.try_recv().is_err());
    let now = crate::client::filetime_now();
    assert!(crate::dao::skills::expert_systems::insert(&f.db, f.actor.character, expert, now, i64::MAX).await.unwrap());
    assert!(crate::services::dispatch(&mut f.client, &call).await.is_err());
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(item)).await.unwrap().pop().unwrap().quantity, 2);
    let installed = crate::dao::skills::expert_systems::all(&f.db, f.actor.character).await.unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!((installed[0].type_id, installed[0].expires_at), (expert, i64::MAX));
    assert!(actor_rx.try_recv().is_err() && owner_rx.try_recv().is_err() && viewer_rx.try_recv().is_err() && outsider_rx.try_recv().is_err());
    assert!(crate::dao::skills::expert_systems::delete(&f.db, f.actor.character, expert, i64::MAX).await.unwrap());
    item_inventory::delete(&f.db, invalid).await.unwrap();
    let empty = transfers::insert(&f.db, expert, f.owner.character, f.host, FLAG_FLEET_HANGAR, 0).await.unwrap();
    let empty_args = [Value::Int(empty)];
    let empty_call = Call { service: "expertSystemMgr", method: "ConsumeExpertSystem", args: &empty_args, kwargs: &kwargs, bound: false, bind_params: None };
    assert!(crate::services::dispatch(&mut f.client, &empty_call).await.is_err());
    assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(empty)).await.unwrap().pop().unwrap().quantity, 0);
    assert!(crate::dao::skills::expert_systems::all(&f.db, f.actor.character).await.unwrap().is_empty());
    assert!(actor_rx.try_recv().is_err() && owner_rx.try_recv().is_err() && viewer_rx.try_recv().is_err() && outsider_rx.try_recv().is_err());
    item_inventory::delete(&f.db, empty).await.unwrap();
    let restricted = f.client.sde.expert_systems.iter().find(|row| row.hidden || row.retired).map(|row| row.type_id);
    if let Some(restricted) = restricted {
        let restricted_item = transfers::insert(&f.db, restricted, f.owner.character, f.host, FLAG_FLEET_HANGAR, 2).await.unwrap();
        let expires = crate::client::filetime_now() + infinity_core::consts::FILETIME_DAY;
        assert!(crate::dao::skills::expert_systems::insert(&f.db, f.actor.character, restricted, now, expires).await.unwrap());
        let restricted_args = [Value::Int(restricted_item)];
        let restricted_call = Call { service: "expertSystemMgr", method: "ConsumeExpertSystem", args: &restricted_args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(crate::services::dispatch(&mut f.client, &restricted_call).await.is_err());
        assert_eq!(load_items(&f.db, &f.client.sde, ItemQuery::Id(restricted_item)).await.unwrap().pop().unwrap().quantity, 2);
        let rows = crate::dao::skills::expert_systems::all(&f.db, f.actor.character).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].type_id, rows[0].expires_at), (restricted, expires));
        assert!(actor_rx.try_recv().is_err() && owner_rx.try_recv().is_err() && viewer_rx.try_recv().is_err() && outsider_rx.try_recv().is_err());
        assert!(crate::dao::skills::expert_systems::delete(&f.db, f.actor.character, restricted, expires).await.unwrap());
        item_inventory::delete(&f.db, restricted_item).await.unwrap();
    }
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    let remaining = load_items(&f.db, &f.client.sde, ItemQuery::Id(item)).await.unwrap().pop().unwrap();
    assert_eq!((remaining.owner_id, remaining.location_id, remaining.quantity), (f.owner.character, f.host, 1));
    let installations = crate::dao::skills::expert_systems::active(&f.db, f.actor.character, crate::client::filetime_now()).await.unwrap();
    assert!(installations.iter().any(|row| row.type_id == expert));
    assert!(crate::dao::skills::expert_systems::all(&f.db, f.owner.character).await.unwrap().is_empty());
    let mut actor_install = false;
    while let Ok((event, _)) = actor_rx.try_recv() { actor_install |= event == "OnExpertSystemsUpdated"; }
    assert!(actor_install);
    let mut owner_changes = 0;
    while let Ok((event, args)) = owner_rx.try_recv() {
        assert_ne!(event, "OnExpertSystemsUpdated");
        if event == "OnItemChange" && args.first().is_some_and(|row| crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(item))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "quantity"), Some(Value::Int(1)));
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(f.host)));
            owner_changes += 1;
        }
    }
    assert_eq!(owner_changes, 1);
    let mut viewer_changes = 0;
    while let Ok((event, args)) = viewer_rx.try_recv() {
        assert_ne!(event, "OnExpertSystemsUpdated");
        if event == "OnItemChange" && args.first().is_some_and(|row| crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(item))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "quantity"), Some(Value::Int(1)));
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(f.host)));
            viewer_changes += 1;
        }
    }
    assert_eq!(viewer_changes, 1);
    assert!(outsider_rx.try_recv().is_err());
    let args = [Value::Int(i64::from(expert))];
    let call = Call { service: "expertSystemMgr", method: "RemoveMyExpertSystem", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    while actor_rx.try_recv().is_ok() {}
    let consume_args = [Value::Int(item)];
    let consume = Call { service: "expertSystemMgr", method: "ConsumeExpertSystem", args: &consume_args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::dispatch(&mut f.client, &consume).await.unwrap();
    assert!(load_items(&f.db, &f.client.sde, ItemQuery::Id(item)).await.unwrap().is_empty());
    let mut removals = 0;
    while let Ok((event, args)) = owner_rx.try_recv() {
        assert_ne!(event, "OnExpertSystemsUpdated");
        if event == "OnItemChange" && args.first().is_some_and(|row| crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(item))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(infinity_core::consts::LOCATION_JUNKYARD)));
            removals += 1;
        }
    }
    assert_eq!(removals, 1);
    let mut viewer_removals = 0;
    while let Ok((event, args)) = viewer_rx.try_recv() {
        assert_ne!(event, "OnExpertSystemsUpdated");
        if event == "OnItemChange" && args.first().is_some_and(|row| crate::services::item_descriptor().get(row, "itemID") == Some(Value::Int(item))) {
            assert_eq!(crate::services::item_descriptor().get(&args[0], "locationID"), Some(Value::Int(infinity_core::consts::LOCATION_JUNKYARD)));
            viewer_removals += 1;
        }
    }
    assert_eq!(viewer_removals, 1);
    assert!(outsider_rx.try_recv().is_err());
    crate::services::dispatch(&mut f.client, &call).await.unwrap();
    f.client.world.unregister_pilot_generation(f.owner.character, None);
    f.client.world.unregister_pilot_generation(f.outsider.character, None);
    f.client.world.unregister_pilot_generation(viewer.character, None);
    for id in [item, hidden, owner_ship, outsider_ship, viewer_ship] { item_inventory::delete(&f.db, id).await.unwrap(); }
    fixture::cleanup(&f.db, viewer).await.unwrap();
    cleanup(f).await;
}
