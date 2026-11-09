//! Fleet authority regressions for bound inventory reads.
//! The fleet protocol mirrors fleetSvc.py; fixtures use the game database and World.
//! Fleet persistence and unrelated inventory operations are not covered here.

use super::*;
use crate::dao::items::ship_configuration::{self, Patch};

pub(super) async fn fleet_rpc(c: &mut Client, id: Option<i64>, method: &str, args: &[Value]) -> CallResult {
    let bind = id.map(Value::Int);
    let kwargs = Value::None;
    crate::services::dispatch(c, &Call { service: "fleetObjectHandler", method, args, kwargs: &kwargs, bound: id.is_some(), bind_params: bind.as_ref() }).await
}

async fn unchanged(f: &BayFixture) {
    let ids = vec![f.pilot, f.host, f.stored, f.mine, f.theirs, f.private, f.own_cargo];
    let rows = crate::dao::items::loading::load_items(&f.db, crate::dao::items::loading::ItemQuery::Ids(&ids)).await.unwrap();
    assert_eq!(rows.len(), ids.len());
    for (id, owner, location, flag) in [
        (f.pilot, f.actor.character, f.system, FLAG_HANGAR),
        (f.host, f.owner.character, f.system, FLAG_HANGAR),
        (f.stored, f.owner.character, f.host, FLAG_SHIP_HANGAR),
        (f.mine, f.actor.character, f.host, FLAG_FLEET_HANGAR),
        (f.theirs, f.owner.character, f.host, FLAG_FLEET_HANGAR),
        (f.private, f.owner.character, f.stored, FLAG_CARGO),
        (f.own_cargo, f.actor.character, f.pilot, FLAG_CARGO),
    ] {
        let row = rows.iter().find(|r| r.item_id == id).unwrap();
        assert_eq!(row.owner_id, owner);
        assert_eq!(row.location_id, location);
        assert_eq!(row.flag_id, flag);
        assert_eq!(row.quantity, 1);
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE and external flock"]
async fn fleet_bay_reads_recheck_toggles_membership_and_reconnect_reservation() {
    let mut f = setup().await;
    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch {
        fleet_hangar_corp: Some(false), maintenance_corp: Some(false),
        fleet_hangar_fleet: Some(true), maintenance_fleet: Some(false), ..Patch::default()
    }).await.unwrap();
    let fh = [Value::Int(i64::from(FLAG_FLEET_HANGAR))];
    let sma = [Value::Int(i64::from(FLAG_SHIP_HANGAR))];
    let cargo = [Value::Int(i64::from(FLAG_CARGO))];
    assert!(rpc(&mut f.client, f.host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.host, "GetItem", &[]).await.is_err());
    unchanged(&f).await;

    let (mut leader, _leader_peer) = crate::services::tests::client(f.db.clone(), f.owner.character).await;
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
    assert!(crate::services::fleets::same_fleet(f.actor.character, f.owner.character));

    assert_eq!(rpc(&mut f.client, f.host, "List", &fh).await.unwrap().items().unwrap().len(), 2);
    assert!(rpc(&mut f.client, f.host, "List", &sma).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.stored, "GetItem", &[]).await.is_err());
    assert!(rpc(&mut f.client, f.host, "GetCapacity", &sma).await.is_err());
    unchanged(&f).await;

    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch {
        fleet_hangar_fleet: Some(false), maintenance_fleet: Some(true), ..Patch::default()
    }).await.unwrap();
    assert!(rpc(&mut f.client, f.host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    assert_eq!(rpc(&mut f.client, f.host, "List", &sma).await.unwrap().items().unwrap().len(), 1);
    assert!(rpc(&mut f.client, f.host, "GetCapacity", &fh).await.is_err());
    assert!(rpc(&mut f.client, f.stored, "GetItem", &[]).await.is_ok());
    assert!(rpc(&mut f.client, f.stored, "List", &cargo).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.stored, "GetCapacity", &cargo).await.is_err());
    unchanged(&f).await;

    ship_configuration::patch(&f.db, f.host, f.owner.character, &Patch {
        fleet_hangar_fleet: Some(true), maintenance_fleet: Some(true), ..Patch::default()
    }).await.unwrap();
    assert_eq!(rpc(&mut f.client, f.host, "List", &[]).await.unwrap().items().unwrap().len(), 3);
    crate::services::fleets::lifecycle::disconnect(&f.client.world, f.actor.character, f.client.session.sid);
    assert!(!crate::services::fleets::same_fleet(f.actor.character, f.owner.character));
    assert!(rpc(&mut f.client, f.host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.host, "List", &sma).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.host, "GetItem", &[]).await.is_err());
    unchanged(&f).await;
    fleet_rpc(&mut f.client, Some(fleet), "Reconnect", &[]).await.unwrap();
    fleet_rpc(&mut f.client, Some(fleet), "AcceptInvite", &[Value::Int(i64::from(f.hull_type))]).await.unwrap();
    assert_eq!(rpc(&mut f.client, f.host, "List", &fh).await.unwrap().items().unwrap().len(), 2);
    fleet_rpc(&mut leader, Some(fleet), "KickMember", &[Value::Int(f.actor.character)]).await.unwrap();
    assert!(!crate::services::fleets::same_fleet(f.actor.character, f.owner.character));
    assert!(rpc(&mut f.client, f.host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.host, "List", &sma).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut f.client, f.host, "GetItem", &[]).await.is_err());
    unchanged(&f).await;
    fleet_rpc(&mut leader, Some(fleet), "DisbandFleet", &[]).await.unwrap();
    f.client.world.unregister_pilot_generation(f.actor.character, None);
    cleanup(f).await;
}
