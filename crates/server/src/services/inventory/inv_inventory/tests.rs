use super::*;
use crate::services::tests::fixture;

pub(super) async fn rpc(c: &mut Client, id: i64, method: &str, args: &[Value]) -> CallResult {
    let bind = Value::tuple([Value::Int(id)]);
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service: super::super::INVENTORY, method, args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await
}

mod fleet;
mod metadata;
mod movement;
mod transactions;
mod hull_lock_order;

pub(super) struct BayFixture {
    pub db: crate::db::Db, pub client: Client, pub peer: tokio::net::TcpStream,
    pub actor: fixture::Fixture, pub owner: fixture::Fixture, pub outsider: fixture::Fixture,
    pub station: i64, pub corp: i64, pub system: i64, pub hull_type: i32, pub commodity: i32,
    pub pilot: i64, pub host: i64, pub stored: i64, pub mine: i64, pub theirs: i64,
    pub private: i64, pub own_cargo: i64,
}

pub(super) fn ball(hull_type: i32, corp: i64, id: i64, who: i64, x: f64, character: Option<i64>) -> crate::space::Ball {
    crate::space::Ball {
        id, type_id: hull_type, owner_id: who, corp_id: corp, alliance_id: 0, char_id: character,
        mode: crate::space::MODE_STOP, radius: 50.0, signature_radius: None, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: vec![],
    }
}
async fn move_fixture_item(db: &crate::db::Db, sde: &Sde, id: i64, owner: i64, location: i64) {
    let item = load_items(db, sde, ItemQuery::Id(id)).await.unwrap().into_iter().next().unwrap();
    assert!(crate::dao::items::inventory::move_item(db, &item, location, item.flag_id, owner, item.singleton).await.unwrap());
}


pub(super) async fn setup() -> BayFixture {
    use infinity_core::consts::dogma::{ATTR_FLEET_HANGAR_CAPACITY, ATTR_SHIP_MAINTENANCE_BAY_CAPACITY};
    use crate::dao::items::{creation, ship_configuration::{self, Patch}, transfers};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let (mut client, peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = client.sde.map.stations().next().unwrap().station_id;
    let corp = fixture::create_corporation(&db, owner.character, station).await.unwrap();
    for character in [actor.character, owner.character] {
        fixture::assign_corporation(&db, character, corp, 0).await.unwrap();
    }
    let system = -101_i64;
    let hull_type = client.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_FLEET_HANGAR_CAPACITY).is_some_and(|v| v > 0.0) && t.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let commodity = client.sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.published == Some(true)).unwrap().type_id;
    let pilot = creation::rookie_ship(&db, hull_type, actor.character, system, FLAG_HANGAR).await.unwrap();
    let host = creation::rookie_ship(&db, hull_type, owner.character, system, FLAG_HANGAR).await.unwrap();
    let stored = creation::rookie_ship(&db, hull_type, owner.character, host, FLAG_SHIP_HANGAR).await.unwrap();
    let mine = transfers::insert(&db, commodity, actor.character, host, FLAG_FLEET_HANGAR, 1).await.unwrap();
    let theirs = transfers::insert(&db, commodity, owner.character, host, FLAG_FLEET_HANGAR, 1).await.unwrap();
    let private = transfers::insert(&db, commodity, owner.character, stored, FLAG_CARGO, 1).await.unwrap();
    let own_cargo = transfers::insert(&db, commodity, actor.character, pilot, FLAG_CARGO, 1).await.unwrap();
    ship_configuration::patch(&db, host, owner.character, &Patch { fleet_hangar_corp: Some(true), maintenance_corp: Some(false), ..Patch::default() }).await.unwrap();
    client.session.apply(vec![("shipid", Value::Int(pilot)), ("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
    client.world.system(system).await.unwrap();
    let pilot_ball = ball(hull_type, corp, pilot, actor.character, 0.0, Some(actor.character));
    let host_ball = ball(hull_type, corp, host, owner.character, 2600.0, None);
    client.world.with(system, move |s| { s.insert(pilot_ball); s.insert(host_ball); }).await.unwrap();
    client.world.observe(system, client.login.client_id, pilot, client.destiny_tx.clone()).await.unwrap();
    BayFixture { db, client, peer, actor, owner, outsider, station, corp, system, hull_type, commodity, pilot, host, stored, mine, theirs, private, own_cargo }
}

pub(super) async fn cleanup(f: BayFixture) {
    f.client.world.unobserve(f.actor.character);
    for id in [f.own_cargo, f.private, f.theirs, f.mine, f.stored, f.host, f.pilot] {
        crate::dao::items::inventory::delete(&f.db, id).await.unwrap();
    }
    fixture::cleanup_corporation(&f.db, f.corp, &[f.actor.character, f.owner.character], 0).await.unwrap();
    fixture::cleanup(&f.db, f.actor).await.unwrap();
    fixture::cleanup(&f.db, f.owner).await.unwrap();
    fixture::cleanup(&f.db, f.outsider).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn shared_bay_dispatch_rechecks_membership_flags_range_and_stored_metadata() {
    use crate::dao::items::ship_configuration::{self, Patch};
    let BayFixture { db, mut client, peer, actor, owner, outsider, station, corp, system, hull_type, commodity, pilot, host, stored, mine, theirs, private, own_cargo } = setup().await;
    let listed = rpc(&mut client, host, "List", &[]).await.unwrap();
    assert_eq!(listed.items().unwrap().len(), 2);
    let fh = [Value::Int(i64::from(FLAG_FLEET_HANGAR))];
    let sma = [Value::Int(i64::from(FLAG_SHIP_HANGAR))];
    let cargo = [Value::Int(i64::from(FLAG_CARGO))];
    assert_eq!(rpc(&mut client, pilot, "List", &cargo).await.unwrap().items().unwrap().len(), 1);
    assert!(rpc(&mut client, pilot, "GetSelfInvItem", &[]).await.is_ok());
    assert_eq!(rpc(&mut client, host, "List", &fh).await.unwrap().items().unwrap().len(), 2);
    assert!(rpc(&mut client, host, "List", &sma).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut client, host, "List", &cargo).await.unwrap().items().unwrap().is_empty());
    let flags = [Value::list([fh[0].clone(), sma[0].clone(), cargo[0].clone()])];
    assert_eq!(rpc(&mut client, host, "ListByFlags", &flags).await.unwrap().items().unwrap().len(), 2);
    let capacity = rpc(&mut client, host, "GetCapacity", &fh).await.unwrap();
    let fit = crate::services::fit::Fit::load_unpiloted(&db, client.sde.clone(), host).await.unwrap();
    let expected = super::super::holds::effective_capacity(&fit, FLAG_FLEET_HANGAR);
    assert_eq!(crate::services::value_field(&capacity, "capacity"), Some(&Value::Float(expected)));
    let volume = crate::services::item_volume::item_volume(&client.sde, commodity, false, 1, None).unwrap();
    assert_eq!(crate::services::value_field(&capacity, "used"), Some(&Value::Float(2.0 * volume)));
    assert!(rpc(&mut client, host, "GetCapacity", &cargo).await.is_err());
    assert!(rpc(&mut client, stored, "GetItem", &[]).await.is_err());
    ship_configuration::patch(&db, host, owner.character, &Patch { maintenance_corp: Some(true), ..Patch::default() }).await.unwrap();
    assert!(rpc(&mut client, stored, "GetItem", &[]).await.is_ok());
    assert!(rpc(&mut client, stored, "GetSelfInvItem", &[]).await.is_ok());
    assert!(rpc(&mut client, stored, "List", &cargo).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut client, stored, "GetCapacity", &cargo).await.is_err());
    assert!(rpc(&mut client, private, "GetItem", &[]).await.is_err());
    ship_configuration::patch(&db, host, owner.character, &Patch { fleet_hangar_corp: Some(false), ..Patch::default() }).await.unwrap();
    assert!(rpc(&mut client, host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    ship_configuration::patch(&db, host, owner.character, &Patch { fleet_hangar_corp: Some(true), ..Patch::default() }).await.unwrap();
    fixture::assign_corporation(&db, actor.character, 0, 0).await.unwrap();
    assert!(rpc(&mut client, host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    assert!(rpc(&mut client, host, "GetItem", &[]).await.is_err());
    for method in ["GetAvailableTurretSlots", "ListSoulBound", "ListServiceLifeDamage"] {
        assert!(rpc(&mut client, host, method, &[]).await.is_err());
    }
    fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    client.session.apply(vec![("stationid", Value::Int(station))]);
    assert!(rpc(&mut client, host, "GetItem", &[]).await.is_err());
    client.session.apply(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system - 1))]);
    assert!(rpc(&mut client, host, "GetItem", &[]).await.is_err());
    client.session.apply(vec![("solarsystemid", Value::Int(system))]);
    move_fixture_item(&db, &client.sde, host, outsider.character, system).await;
    assert!(rpc(&mut client, host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    move_fixture_item(&db, &client.sde, host, owner.character, system).await;
    move_fixture_item(&db, &client.sde, host, owner.character, system - 1).await;
    assert!(rpc(&mut client, host, "GetItem", &[]).await.is_err());
    move_fixture_item(&db, &client.sde, host, owner.character, system).await;
    let mut hidden = ball(hull_type, corp, host, owner.character, 2600.0, None);
    hidden.cloak = 1;
    client.world.with(system, move |s| { s.insert(hidden); }).await.unwrap();
    assert!(rpc(&mut client, host, "GetItem", &[]).await.is_err());
    let far_ball = ball(hull_type, corp, host, owner.character, 2600.01, None);
    client.world.with(system, move |s| { s.insert(far_ball); }).await.unwrap();
    assert!(rpc(&mut client, host, "List", &fh).await.unwrap().items().unwrap().is_empty());
    crate::dao::items::inventory::delete(&db, host).await.unwrap();
    for method in ["List", "GetItem", "GetSelfInvItem", "GetCapacity", "GetAvailableTurretSlots", "ListSoulBound", "ListServiceLifeDamage"] {
        assert!(rpc(&mut client, host, method, &[]).await.is_err());
    }
    cleanup(BayFixture { db, client, peer, actor, owner, outsider, station, corp, system, hull_type, commodity, pilot, host, stored, mine, theirs, private, own_cargo }).await;
}
