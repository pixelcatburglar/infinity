//! Probe launch against the real loaded-hold gate: the launcher+probe pair is
//! picked from SDE data and filtered by the production `charge_matches` rule;
//! a competing boundary hold refuses and a released hold launches.
use super::*;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use infinity_wire::macho::Address;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn launch_probes_refuses_a_competing_loaded_hold_then_launches() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = 910_009_i64;
    let size_id = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
    let probe_type = sde.types.iter().find(|ty| ty.published == Some(true)
        && ty.group_id == Some(infinity_core::consts::GROUP_SCANNER_PROBE)).expect("SDE scanner probe");
    let launcher = sde.types.iter().find(|ty| ty.published == Some(true)
        && ty.group_id.and_then(|g| sde.types.group(g)).is_some_and(|g| g.lower_name.as_deref().is_some_and(|n| n.contains("probe launcher")))
        && charge_matches(CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| ty.attribute(a.attribute_id)),
            probe_type.group_id,
            size_id.and_then(|id| ty.attribute(id)),
            size_id.and_then(|id| probe_type.attribute(id)))).expect("compatible probe launcher").type_id;
    let ship_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system }, &[
        ShipItemSpec { type_id: launcher, flag_id: 27, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let (ship, module, probe) = (fixture.ship, fixture.items[0], probe_type.type_id);
    crate::dao::items::combat::apply_loads(&db, ship, &[(module, 27, probe, 8, 0)]).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    c.session.apply(vec![("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
    c.world.system(system).await.unwrap();
    // The resident record comes from the real fit: the launcher module node
    // and its charge seed are loaded by production fitting topology.
    let fit = crate::services::fit::Fit::load(&db, sde.clone(), fixture.character, ship).await.unwrap();
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    c.world.with(system, move |s| s.install_condition(ship, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit)).await.unwrap();
    let (kwargs, args) = (Value::Dict(vec![]), vec![Value::Int(module), Value::Int(1)]);
    let call = Call { service: "dogmaIM", method: "LaunchProbes", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    // A competing boundary hold makes the probe root refuse with its own busy error.
    let hold = crate::space::system::boarding::quiesce_ship_loaded(&c.world, system, &[ship]).await.unwrap();
    let Err(CallError::User { dict, .. }) = crate::services::exploration::launch_probes(&mut c, &call).await else { panic!("expected busy refusal") };
    assert!(dict.iter().any(|(k, v)| k.as_str() == "notify" && v.as_text().is_some_and(|t| t.trim() == messages::fitting::CHARGES_SETTLING)),
        "the refusal carries the charges-settling notify");
    assert_eq!(crate::dao::items::charges::loaded(&db, ship, 27).await.unwrap().unwrap().quantity, 8);
    drop(hold);
    for _ in 0..200 {
        if c.world.with(system, move |s| s.loaded_effective(ship, module)).await.flatten().is_some() { break; }
        tokio::task::yield_now().await;
    }
    crate::services::exploration::launch_probes(&mut c, &call).await.unwrap();
    assert_eq!(crate::dao::items::charges::loaded(&db, ship, 27).await.unwrap().unwrap().quantity, 7);
    assert!(c.outbox.iter().any(|p| matches!(&p.destination, Address::Broadcast { broadcast_id, .. }
        if broadcast_id.as_text().as_deref() == Some("OnNewProbe"))), "the launch published a real probe");
    cleanup_ship(&db, Some(&*c.world), fixture).await.unwrap();
    // The launched probe stays in exploration's private state().probes — inert: unique char/system per fixture.
}

/// A forged `LaunchProbes` naming an interdiction sphere launcher, or a scan
/// launcher whose loaded charge is not a scanner probe, is refused before any
/// charge is consumed or scanner probe state is created.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn launch_probes_rejects_non_scan_launcher_and_charge() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = 910_010_i64;
    let bubble_launcher = sde.types.iter().find(|ty| ty.published == Some(true) && ty.group_id == Some(589))
        .expect("SDE bubble probe launcher").type_id;
    let warp_probe = sde.types.iter().find(|ty| ty.published == Some(true) && ty.group_id == Some(548))
        .expect("SDE warp disruption probe").type_id;
    let scan_launcher = sde.types.iter().find(|ty| ty.published == Some(true)
        && ty.group_id.and_then(|g| sde.types.group(g)).is_some_and(|g| g.lower_name.as_deref().is_some_and(|n| n.contains("scan probe launcher"))))
        .expect("SDE scan probe launcher").type_id;
    let ship_type = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let specs = [
        ShipItemSpec { type_id: bubble_launcher, flag_id: 27, quantity: 1, singleton: true, online: true, attributes: &[] },
        ShipItemSpec { type_id: scan_launcher, flag_id: 28, quantity: 1, singleton: true, online: true, attributes: &[] },
    ];
    let fixture = create_ship(&db, ship_type, ShipLocation::Space { system }, &specs).await.unwrap();
    let (ship, bubble, scan) = (fixture.ship, fixture.items[0], fixture.items[1]);
    crate::dao::items::combat::apply_loads(&db, ship, &[(bubble, 27, warp_probe, 4, 0), (scan, 28, warp_probe, 4, 0)]).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    c.session.apply(vec![("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system))]);
    c.world.system(system).await.unwrap();
    for (module, flag) in [(bubble, 27), (scan, 28)] {
        let (kwargs, args) = (Value::Dict(vec![]), vec![Value::Int(module), Value::Int(1)]);
        let call = Call { service: "dogmaIM", method: "LaunchProbes", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        crate::services::exploration::launch_probes(&mut c, &call).await.unwrap();
        assert_eq!(crate::dao::items::charges::loaded(&db, ship, flag).await.unwrap().unwrap().quantity, 4,
            "module {module} must keep every charge");
    }
    assert!(!c.outbox.iter().any(|p| matches!(&p.destination, Address::Broadcast { broadcast_id, .. }
        if broadcast_id.as_text().as_deref() == Some("OnNewProbe"))), "no scanner probe may be minted");
    cleanup_ship(&db, Some(&*c.world), fixture).await.unwrap();
}
