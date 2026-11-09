use std::{collections::HashSet, future::Future, task::Poll};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::{services::{Call, fit::{Fit, Key}, effects::{EffectKind, registry}}, space::drone::DroneOrder};
use super::fixture::Fixture;

async fn catch_scenario(future: impl Future<Output = ()>) -> std::thread::Result<()> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
        Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
        Ok(Poll::Pending) => Poll::Pending,
        Err(panic) => Poll::Ready(Err(panic)),
    }).await
}


fn combat_type(f: &Fixture) -> i32 {
    f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
        && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
        && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
        .expect("published turret drone").type_id
}

async fn returned(f: &Fixture, ids: &[i64]) {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let count: i64 = f.db.query_one("SELECT count(*) FROM game.items WHERE item_id=ANY($1) AND owner_id=$2 AND location_id=$3 AND flag_id=$4 AND singleton", &[&ids, &f.owner, &f.ship, &FLAG_DRONE_BAY]).await.unwrap().get("count");
            if count == ids.len() as i64 { break; }
            assert!(f.world.after(f.system, 100).await);
        }
    }).await.expect("drone return contact");
    for &id in ids { assert!(f.world.ball(f.system, id).await.is_none()); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn launch_limits_and_return_are_inventory_backed() {
    let mut f = Fixture::new().await;
    let result = catch_scenario(async {
    let ty = combat_type(&f);
    f.train_drone(ty).await;
    let source = f.drone(ty, 5, false).await;
    let fit = Fit::drone(&f.db, f.sde.clone(), f.owner, source).await.ok().unwrap();
    assert_eq!(fit.value(Key::Char, ATTR_MAX_ACTIVE_DRONES), 5.0);
    let bandwidth = fit.value(Key::Drone(source), ATTR_DRONE_BANDWIDTH_USED);
    assert!(bandwidth > 0.0);
    f.set(f.ship, ATTR_DRONE_BANDWIDTH, 4.0 * bandwidth).await;
    f.set(f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(f.owner, tx);
    let args = [Value::list([Value::tuple([Value::Int(source), Value::Int(5)])]), Value::None, Value::Bool(false)];
    let Value::Dict(refused) = f.call("ship", "LaunchDrones", &args).await.ok().unwrap() else { panic!("launch mapping") };
    let error = Value::list([Value::tuple([Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(infinity_core::messages::drones::BANDWIDTH_EXCEEDED))])])]);
    assert_eq!(refused, vec![(Value::Int(source), error.clone())]);
    let row = f.db.query_one("SELECT location_id,quantity,singleton FROM game.items WHERE item_id=$1", &[&source]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), f.ship);
    assert_eq!(row.get::<_, i64>("quantity"), 5);
    assert!(!row.get::<_, bool>("singleton"));
    let ship = f.ship;
    assert_eq!(f.world.with(f.system, move |s| s.drones().filter(|d| d.controller == Some(ship)).count()).await.unwrap(), 0);
    f.set(f.ship, ATTR_DRONE_BANDWIDTH, 5.0 * bandwidth).await;
    let Value::Dict(rows) = f.call("ship", "LaunchDrones", &args).await.ok().unwrap() else { panic!("launch mapping") };
    let Value::List(values) = &rows[0].1 else { panic!("launch failed: {:?}", rows[0].1) };
    let ids: Vec<i64> = values.iter().map(|v| v.as_int().unwrap()).collect();
    assert_eq!(ids.len(), 5);
    assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), 5);
    assert!(ids.contains(&source));
    let system = f.system; let owner = f.owner; let ship = f.ship;
    let actor_ids = ids.clone();
    assert!(f.world.with(system, move |s| s.pilot_ball(owner).is_some_and(|b| b.id == ship) && s.drones().filter(|d| d.controller == Some(ship)).count() == 5 && actor_ids.iter().all(|id| s.ball(*id).is_some_and(|b| b.char_id.is_none() && b.owner_id == owner))).await.unwrap());
    for id in &ids {
        let row = f.db.query_one("SELECT owner_id,location_id,singleton,quantity,x,y,z FROM game.items WHERE item_id=$1", &[id]).await.unwrap();
        assert_eq!(row.get::<_, i64>("owner_id"), f.owner); assert_eq!(row.get::<_, i64>("location_id"), f.system);
        assert!(row.get::<_, bool>("singleton")); assert_eq!(row.get::<_, i64>("quantity"), 1);
        for col in ["x", "y", "z"] { assert!(row.get::<_, Option<f64>>(col).is_some()); }
    }
    f.set(f.ship, ATTR_DRONE_BANDWIDTH, 6.0 * bandwidth).await;
    let extra = f.drone(ty, 1, false).await;
    let args = [Value::list([Value::tuple([Value::Int(extra), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let Value::Dict(rows) = f.call("ship", "LaunchDrones", &args).await.ok().unwrap() else { panic!("launch mapping") };
    assert_eq!(rows, vec![(Value::Int(extra), error)]);
    assert_eq!(f.db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&extra]).await.unwrap().get::<_, i64>("location_id"), f.ship);
    assert_eq!(f.world.with(system, move |s| s.drones().filter(|d| d.controller == Some(ship)).count()).await.unwrap(), 5);
    let damaged = ids[0];
    let checkpoint = f.world.with(system, move |s| {
        assert!(s.set_condition_fraction(damaged, 0.5));
        s.combat_condition_snapshot(damaged).unwrap().condition
    }).await.unwrap();
    let commands = [Value::list(ids.iter().copied().map(Value::Int))];
    assert_eq!(f.call("entity", "CmdReturnHome", &commands).await.ok().unwrap(), Value::Dict(vec![]));
    assert!(f.world.with(system, { let ids=ids.clone(); move |s| ids.iter().all(|id| s.drone(*id).is_some_and(|d| d.order == DroneOrder::ReturnHome)) }).await.unwrap());
    assert_eq!(f.db.query_one("SELECT count(*) FROM game.items WHERE item_id=ANY($1) AND location_id=$2", &[&ids, &system]).await.unwrap().get::<_, i64>("count"), 5);
    for method in ["CmdSalvage", "CmdReconnectToDrones"] { assert!(f.call("entity", method, &commands).await.is_err()); }
    f.world.with(system, move |s| s.drone_controller_lost(ship)).await.unwrap();
    for id in &ids { assert!(f.world.with(system, { let id=*id; move |s| s.drone(id).is_some_and(|d| d.owner == owner && d.controller.is_none()) }).await.unwrap()); }
    let mut lost = HashSet::new();
    while let Ok((event, args)) = rx.try_recv() { if event == "OnDroneControlLost" { if let Some(id) = args.first().and_then(Value::as_int) { lost.insert(id); } } }
    assert_eq!(lost, ids.iter().copied().collect());
    assert_eq!(f.call("entity", "CmdReturnBay", &commands).await.ok().unwrap(), Value::Dict(vec![]));
    returned(&f, &ids).await;
    let saved = crate::dao::items::dogma::overrides(&f.db, damaged).await.unwrap();
    let shield = saved.iter().find(|row| row.attribute_id == ATTR_SHIELD_CHARGE).unwrap();
    let armor = saved.iter().find(|row| row.attribute_id == ATTR_ARMOR_DAMAGE).unwrap();
    let hull = saved.iter().find(|row| row.attribute_id == ATTR_HULL_DAMAGE).unwrap();
    assert!(shield.value >= checkpoint.now[0] && shield.updated_at >= checkpoint.shield_at);
    assert!((armor.value - (checkpoint.stats.max[1] - checkpoint.now[1])).abs() < 1e-6);
    assert!((hull.value - (checkpoint.stats.max[2] - checkpoint.now[2])).abs() < 1e-6);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn combat_drone_and_npc_death_share_real_effect_path() {
    let mut f = Fixture::new().await;
    let npc_source = crate::dao::map_telemetry::EventSource::NpcOccurrence as i16;
    let prior_occurrences: HashSet<i64> = f.db.query("SELECT source_id FROM game.map_events WHERE source=$1 AND solar_system_id=$2", &[&npc_source, &f.system]).await.unwrap().iter().map(|row| row.get("source_id")).collect();
    let result = catch_scenario(async {
    let ty = combat_type(&f);
    f.train_drone(ty).await;
    let drone = f.drone(ty, 1, true).await;
    for (item, attr, value) in [
        (f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
        (f.ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
        (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
        (drone, ATTR_EM_DAMAGE, 1_000_000_000.0),
        (drone, ATTR_DAMAGE_MULTIPLIER, 1.0),
        (drone, ATTR_MAX_RANGE, 1_000_000.0),
        (drone, ATTR_TRACKING_SPEED, 1_000_000.0),
        (drone, ATTR_SHIELD_CAPACITY, 0.01),
        (drone, ATTR_ARMOR_HP, 0.01),
        (drone, ATTR_STRUCTURE_HP, 0.01),
        (drone, ATTR_SIGNATURE_RADIUS, 10000.0),
        (drone, ATTR_RATE_OF_FIRE, 100.0),
    ] { f.set(item, attr, value).await; }
    let npc_data = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)
        && t.attribute(ATTR_ENTITY_KILL_BOUNTY).is_some_and(|v| v > 0.0)
        && [ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE].iter().any(|attr| t.attribute(*attr).is_some_and(|v| v > 0.0))
        && t.attribute(ATTR_DAMAGE_MULTIPLIER).unwrap_or(1.0) > 0.0
        && t.attribute(ATTR_SCAN_RESOLUTION).is_some_and(|v| v > 0.0)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.group_id.and_then(|group| f.sde.wrecks.get(group, t.race_id.unwrap_or(0))).is_some())
        .expect("bounty-bearing NPC with wreck");
    let npc_type = npc_data.type_id;
    let wreck_type = f.sde.wrecks.get(npc_data.group_id.unwrap(), npc_data.race_id.unwrap_or(0)).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    f.world.register_pilot(f.owner, tx);
    let args = [Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)];
    let Value::Dict(launched) = f.call("ship", "LaunchDrones", &args).await.ok().unwrap() else { panic!("launch mapping") };
    assert_eq!(launched[0].1, Value::list([Value::Int(drone)]));
    let npc = crate::services::combat::npc::spawn(&f.db, &f.world, f.system, f.sde.types.get(npc_type).unwrap(), 1, [1000.0, 0.0, 0.0], crate::space::now_ms(), None).await.ok().unwrap();
    let kwargs = Value::None;
    let args = [Value::Int(npc)];
    let lock = Call { service: "dogmaIM", method: "AddTarget", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dogma::dispatch(&mut f.client, &lock).await.ok().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !crate::services::combat::targeting_charges::targets_of(f.owner).contains(&npc) {
            assert!(f.world.after(f.system, 100).await);
        }
    }).await.expect("target lock");
    let balance = crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap();
    let args = [Value::list([Value::Int(drone)]), Value::Int(npc)];
    assert_eq!(f.call("entity", "CmdEngage", &args).await.ok().unwrap(), Value::Dict(vec![]));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while f.world.ball(f.system, npc).await.is_some() { assert!(f.world.after(f.system, 100).await); }
        while crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap() <= balance { assert!(f.world.after(f.system, 100).await); }
    }).await.expect("drone NPC kill and bounty");
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while f.db.query_opt("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3", &[&f.owner, &f.system, &wreck_type]).await.unwrap().is_none() {
            assert!(f.world.after(f.system, 100).await);
        }
    }).await.expect("NPC wreck");
    let post_kill = crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap();
    let before_items: i64 = f.db.query_one("SELECT count(*) FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&f.owner, &f.system]).await.unwrap().get("count");
    let pos = f.world.ball(f.system, drone).await.unwrap().pos;
    let attacker = crate::services::combat::npc::spawn(&f.db, &f.world, f.system, f.sde.types.get(npc_type).unwrap(), 1, pos, crate::space::now_ms(), Some(f.owner)).await.ok().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while f.world.ball(f.system, drone).await.is_some() {
            assert!(f.world.ball(f.system, attacker).await.is_some());
            assert!(f.world.after(f.system, 100).await);
        }
        while f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&drone]).await.unwrap().is_some() {
            assert!(f.world.after(f.system, 100).await);
        }
    }).await.expect("NPC brain destroys drone");
    assert!(f.db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&drone]).await.unwrap().is_none());
    assert!(f.db.query("SELECT item_id FROM game.item_attributes WHERE item_id=$1", &[&drone]).await.unwrap().is_empty());
    assert_eq!(crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap(), post_kill);
    assert_eq!(f.db.query_one("SELECT count(*) FROM game.items WHERE owner_id=$1 AND location_id=$2", &[&f.owner, &f.system]).await.unwrap().get::<_, i64>("count"), before_items - 1);
    let mut death_state = false;
    let mut physical_source = false;
    while let Ok((event, args)) = rx.try_recv() {
        if event == "OnDamageMessage" {
            if let Some(Value::Dict(fields)) = args.first() {
                physical_source |= fields.iter().any(|(key, value)| *key == Value::str("source") && *value == Value::Int(attacker));
            }
        }
        if event == "OnDroneStateChange" && args.first().and_then(Value::as_int) == Some(drone)
            && args.get(1) == Some(&Value::None) && args.get(2) == Some(&Value::None) { death_state = true; }
    }
    assert!(death_state);
    assert!(physical_source);
    }).await;
    let occurrences: Vec<i64> = f.db.query("SELECT source_id FROM game.map_events WHERE source=$1 AND solar_system_id=$2", &[&npc_source, &f.system]).await.unwrap().iter().map(|row| row.get("source_id")).filter(|id| !prior_occurrences.contains(id)).collect();
    f.db.execute("DELETE FROM game.map_events WHERE source=$1 AND solar_system_id=$2 AND source_id=ANY($3)", &[&npc_source, &f.system, &occurrences]).await.unwrap();
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
