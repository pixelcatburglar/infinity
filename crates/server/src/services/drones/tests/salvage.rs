//! Salvage drone coverage through real commands, the actor cycle and recovery.
use anyhow::{anyhow, ensure, Context, Result};
use marshal::Value;
use infinity_core::{consts::{*, dogma::*}};
use crate::dao::items::{loading::{self, ItemQuery}, transfers};
use crate::services::{Call, drones::worker, effects::tests::fixture as effect_fixture, items::load_items};
use crate::space::drone::DroneOrder;
use super::{fixture::Fixture, shared::{launch_args, launch_salvager, salvage_types, wait_until, wreck}};

const TICK: u64 = 100;

async fn command(f: &mut Fixture, method: &str, ids: &[i64], target: Option<Value>) -> Result<Value> {
    let mut args = vec![Value::list(ids.iter().copied().map(Value::Int))];
    args.extend(target);
    f.call("entity", method, &args).await.map_err(|e| anyhow!("{method}: {e:?}"))
}

/// The real targeting path: publish lock attrs, `dogmaIM AddTarget`, wait.
async fn lock(f: &mut Fixture, target: i64) -> Result<()> {
    for attr in [ATTR_MAX_TARGET_RANGE, ATTR_MAX_LOCKED_TARGETS, ATTR_SCAN_RESOLUTION] {
        effect_fixture::gm_value(&mut f.client, f.ship, attr, 1_000_000.0).await;
    }
    let args = [Value::Int(target)]; let kwargs = Value::None;
    let call = Call { service: "dogmaIM", method: "AddTarget", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::dogma::dispatch(&mut f.client, &call).await.map_err(|e| anyhow!("target lock: {e:?}"))?;
    let owner = f.owner;
    wait_until(f, "target lock", move |_| crate::services::combat::targeting_charges::targets_of(owner).contains(&target).then_some(())).await?;
    Ok(())
}

fn ids(items: &[loading::Item]) -> Vec<i64> { items.iter().map(|i| i.item_id).collect() }

/// Poll a location until `until` accepts its rows, pacing on real ticks.
async fn wait_items(f: &Fixture, location: i64, flags: &[i32], what: &str, until: impl Fn(&[loading::Item]) -> bool + Send + Sync + 'static) -> Result<Vec<loading::Item>> {
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let items = load_items(&f.db, &f.sde, match flags.is_empty() { true => ItemQuery::Location(location), false => ItemQuery::LocationFlags(location, flags) }).await?;
            if until(&items) { return Ok::<_, anyhow::Error>(items); }
            ensure!(f.world.after(f.system, TICK).await, "system stopped");
        }
    }).await.map_err(|_| anyhow!("{what} timed out"))?
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn salvage_orders_transport_custody_and_restore() -> Result<()> {
    let mut f = Fixture::new().await;
    let result: Result<()> = async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _pilot_binding = f.world.bind_pilot(f.owner, tx.clone(), None, None).map_err(|e| anyhow!("bind pilot: {e:?}"))?;
        f.world.register_pilot(f.owner, tx);
        let ship = f.ship;
        let ball = f.world.with(f.system, move |s| s.sampled_ball(ship)).await.flatten().ok_or_else(|| anyhow!("fixture ship missing"))?;
        effect_fixture::admit(&f.db, &f.world, f.system, ball).await;
        let role = f.db.query_one("SELECT role FROM game.accounts WHERE user_id=$1", &[&f.account]).await?.get::<_, i64>("role");
        ensure!(role == 0 && f.db.query("SELECT 1 FROM game.corporations WHERE corporation_id=1", &[]).await?.is_empty(),
            "fixture is not a plain NPC-corporation account");
        let (ty, dur, rng, wreck_ty, loot_ty) = salvage_types(&f.sde);
        f.train_drone(ty).await;
        effect_fixture::gm_value(&mut f.client, f.ship, ATTR_CAPACITY, 1_000_000.0).await;
        let (guest, guest_ship) = f.add_pilot().await;
        let foreign = wreck(&f, wreck_ty, guest, 2_000.0).await?;
        let far = wreck(&f, wreck_ty, f.owner, 6_000.0).await?;
        let near = wreck(&f, wreck_ty, f.owner, 4_000.0).await?;
        let loot_id = transfers::insert(&f.db, loot_ty, f.owner, near, 0, 3).await?;
        let d1 = launch_salvager(&mut f, ty, dur, rng, 1_000_000.0, 400.0).await?;
        let d2 = launch_salvager(&mut f, ty, dur, rng, 1_000_000.0, 400.0).await?;
        // One batch focuses both drones on the nearest owned wreck.
        ensure!(command(&mut f, "CmdSalvage", &[d1, d2], Some(Value::None)).await? == Value::Dict(vec![]), "salvage refused");
        for d in [d1, d2] {
            ensure!(f.world.with(f.system, move |s| s.drone(d).is_some_and(|d| d.target == Some(near) && d.order == DroneOrder::Salvage(None))).await.context("system stopped")?, "auto pick failed for {d}");
        }
        wait_until(&f, "cargo committed aboard", move |s| (s.drone_carries_cargo(d1) || s.drone_carries_cargo(d2)).then_some(())).await?;
        let aboard = load_items(&f.db, &f.sde, ItemQuery::Location(d1)).await?.into_iter()
            .chain(load_items(&f.db, &f.sde, ItemQuery::Location(d2)).await?).collect::<Vec<_>>();
        ensure!(aboard.iter().all(|i| i.owner_id == f.owner && i.flag_id == FLAG_CARGO && i.quantity > 0 && i.group_id == i64::from(GROUP_SALVAGED_MATERIALS)),
            "unexpected drone cargo");
        ensure!(load_items(&f.db, &f.sde, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await?.is_empty(), "ship received cargo before contact");
        let completed = wait_until(&f, "both wrecks recovered", move |s| (s.drone(d1).is_some_and(|d| d.state == DRONE_STATE_IDLE && d.target.is_none())
            && s.drone(d2).is_some_and(|d| d.state == DRONE_STATE_IDLE && d.target.is_none())).then_some(())).await;
        if let Err(error) = completed {
            let snapshot = f.world.with(f.system, move |s| {
                ([d1, d2].map(|id| (id, s.drone(id).map(|d| (d.order, d.state, d.target, d.generation)), s.drone_carries_cargo(id), s.drone_delivery_blocked(id))),
                    s.ball(near).is_some(), s.ball(far).is_some(), s.with_combat_fit(ship, |fit, _, _| fit.value(crate::services::fit::Key::Ship, ATTR_CAPACITY)))
            }).await;
            let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).filter(|(name, _)| name == "OnRemoteMessage").collect();
            return Err(error.context(format!("recovery snapshot (drones, near, far, capacity): {snapshot:?}; remote={notices:?}")));
        }
        let held = load_items(&f.db, &f.sde, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await?;
        ensure!(aboard.iter().all(|b| held.iter().any(|i| (i.item_id, i.type_id, i.owner_id, i.quantity, i.singleton)
            == (b.item_id, b.type_id, b.owner_id, b.quantity, b.singleton))), "delivery changed the rows");
        ensure!(load_items(&f.db, &f.sde, ItemQuery::Location(d1)).await?.is_empty()
            && load_items(&f.db, &f.sde, ItemQuery::Location(d2)).await?.is_empty(), "cargo left aboard");
        ensure!(load_items(&f.db, &f.sde, ItemQuery::Id(near)).await?.is_empty()
            && load_items(&f.db, &f.sde, ItemQuery::Id(far)).await?.is_empty(), "wreck rows survive their claims");
        ensure!(!load_items(&f.db, &f.sde, ItemQuery::Id(foreign)).await?.is_empty(), "foreign-owned wreck was touched");
        let loot = load_items(&f.db, &f.sde, ItemQuery::Id(loot_id)).await?.pop().context("loot vanished")?;
        let container = load_items(&f.db, &f.sde, ItemQuery::Id(loot.location_id)).await?.pop().context("loot container missing")?;
        ensure!(container.group_id == i64::from(GROUP_CARGO_CONTAINER) && container.location_id == f.system, "loot not preserved in a positioned container");
        ensure!((loot.type_id, loot.owner_id, loot.flag_id, loot.quantity) == (loot_ty, f.owner, 0, 3), "loot row changed");
        let events = crate::db::outbox::tests::events(&f.db, "WreckSalvaged", f.owner).await;
        ensure!(events.len() == 2 && events.iter().all(|e| matches!(e, crate::lua::events::GameEvent::WreckSalvaged { char_id, context, .. }
            if *char_id == f.owner && context.ship_id == Some(f.ship) && context.system_id == Some(f.system))), "events: {events:?}");
        let journal = f.db.query_one("SELECT source_namespace, source_key FROM game.progress_events WHERE kind=$1 AND subject_character_id=$2 AND source_key=$3",
            &[&"WreckSalvaged", &f.owner, &format!("WreckSalvaged:{near}")]).await?;
        ensure!(journal.get::<_, String>("source_namespace") == "business", "business source missing");
        let w3 = wreck(&f, wreck_ty, f.owner, 4_500.0).await?;
        let w4 = wreck(&f, wreck_ty, f.owner, 3_000.0).await?;
        lock(&mut f, w3).await?;
        ensure!(command(&mut f, "CmdSalvage", &[d1], Some(Value::Int(w3))).await? == Value::Dict(vec![]), "manual salvage refused");
        wait_until(&f, "standing intent retargets", move |s| (s.drone(d1).is_some_and(|d| d.target == Some(w4))).then_some(())).await?;
        wait_until(&f, "intent stands down", move |s| (s.drone(d1)
            .is_some_and(|d| d.order == DroneOrder::Salvage(Some(w3)) && d.state == DRONE_STATE_IDLE && d.target.is_none())).then_some(())).await?;
        ensure!(command(&mut f, "CmdReturnHome", &[d1, d2], None).await? == Value::Dict(vec![]), "return refused");
        wait_until(&f, "returned", move |s| (s.drone(d1).is_some_and(|d| d.order == DroneOrder::Idle)).then_some(())).await?;
        let w5 = wreck(&f, wreck_ty, f.owner, 4_600.0).await?;
        let w6 = wreck(&f, wreck_ty, f.owner, 4_300.0).await?;
        lock(&mut f, w5).await?;
        ensure!(command(&mut f, "CmdSalvage", &[d1], Some(Value::Int(w5))).await? == Value::Dict(vec![]), "manual salvage refused");
        wait_until(&f, "manual completion", move |s| (s.drone(d1).is_some_and(|d| d.order == DroneOrder::Idle)).then_some(())).await?;
        ensure!(load_items(&f.db, &f.sde, ItemQuery::Id(w6)).await?.len() == 1, "a cleared intent still retargeted to w6");
        let d3 = launch_salvager(&mut f, ty, dur, rng, -1_000_000.0, 400.0).await?;
        let w7 = wreck(&f, wreck_ty, f.owner, 4_400.0).await?;
        lock(&mut f, w7).await?;
        ensure!(command(&mut f, "CmdSalvage", &[d3], Some(Value::Int(w7))).await? == Value::Dict(vec![]), "salvage refused");
        wait_until(&f, "working the wreck", move |s| (s.drone(d3)
            .is_some_and(|d| d.target == Some(w7) && d.state == DRONE_STATE_SALVAGING)).then_some(())).await?;
        for _ in 0..10 { ensure!(f.world.after(f.system, TICK).await, "system stopped"); }
        ensure!(load_items(&f.db, &f.sde, ItemQuery::Id(w7)).await?.len() == 1, "missed wreck was claimed");
        ensure!(load_items(&f.db, &f.sde, ItemQuery::Location(d3)).await?.is_empty()
            && f.world.with(f.system, move |s| !s.drone_carries_cargo(d3)).await.context("system stopped")?, "a miss produced cargo");
        ensure!(crate::db::outbox::tests::events(&f.db, "WreckSalvaged", f.owner).await.len() == 5, "a miss journaled a claim");
        let junk_ty = f.sde.types.iter().find(|t| t.group_id == Some(GROUP_CARGO_CONTAINER)).context("container type")?.type_id;
        let junk = wreck(&f, junk_ty, f.owner, 3_000.0).await?;
        lock(&mut f, junk).await?;
        let response = command(&mut f, "CmdSalvage", &[d3], Some(Value::Int(junk))).await?;
        ensure!(matches!(&response, Value::Dict(rows) if rows.len() == 1 && rows[0].0 == Value::Int(d3) && matches!(&rows[0].1, Value::Tuple(_))),
            "non-wreck target accepted: {response:?}");
        // A full hold refuses the whole delivery and holds the drone in place.
        effect_fixture::gm_value(&mut f.client, f.ship, ATTR_CAPACITY, 0.0).await;
        let d4 = launch_salvager(&mut f, ty, dur, rng, 1_000_000.0, 400.0).await?;
        ensure!(command(&mut f, "CmdSalvage", &[d4], Some(Value::None)).await? == Value::Dict(vec![]), "salvage refused");
        let aboard = wait_items(&f, d4, &[], "cargo aboard", |i| !i.is_empty()).await?;
        wait_until(&f, "delivery held", move |s| s.drone_delivery_blocked(d4).then_some(())).await?;
        ensure!(ids(&load_items(&f.db, &f.sde, ItemQuery::Location(d4)).await?) == ids(&aboard)
            && !load_items(&f.db, &f.sde, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await?.iter().any(|i| aboard.iter().any(|b| b.item_id == i.item_id)), "refused delivery moved rows");
        // The refusal is told once and nothing retries on its own.
        let mut told = 0u32;
        for _ in 0..20 {
            while let Ok((name, args)) = rx.try_recv() {
                if name == "OnRemoteMessage" && args.first() == Some(&Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY))
                    && args.get(1).and_then(|v| v.get("notify")) == Some(&Value::unicode(infinity_core::messages::CARGO_HOLD_FULL)) { told += 1; }
            }
            ensure!(f.world.after(f.system, TICK).await, "system stopped");
        }
        ensure!(told == 1 && ids(&load_items(&f.db, &f.sde, ItemQuery::Location(d4)).await?) == ids(&aboard), "cargo refusal repeated or moved: told={told}");
        // With room again, the next player command retries the delivery once.
        effect_fixture::gm_value(&mut f.client, f.ship, ATTR_CAPACITY, 1_000_000.0).await;
        ensure!(command(&mut f, "CmdSalvage", &[d4], Some(Value::None)).await? == Value::Dict(vec![]), "retry refused");
        let aboard_ids = ids(&aboard);
        let held = wait_items(&f, f.ship, &[FLAG_CARGO], "cargo delivered", move |held|
            aboard_ids.iter().all(|b| held.iter().any(|i| i.item_id == *b))).await?;
        ensure!(aboard.iter().all(|b| held.iter().any(|i| (i.type_id, i.owner_id, i.quantity, i.singleton)
            == (b.type_id, b.owner_id, b.quantity, b.singleton))), "redelivery changed the rows");
        // Abandon keeps the cargo aboard; a third party cannot scoop foreign cargo.
        ensure!(command(&mut f, "CmdSalvage", &[d4], Some(Value::None)).await? == Value::Dict(vec![]), "salvage refused");
        let aboard = wait_items(&f, d4, &[], "cargo aboard", |i| !i.is_empty()).await?;
        ensure!(command(&mut f, "CmdAbandonDrone", &[d4], None).await? == Value::Dict(vec![]), "abandon refused");
        ensure!(f.db.query_one("SELECT owner_id FROM game.items WHERE item_id=$1", &[&d4]).await?.get::<_, i64>("owner_id") == OWNER_UNOWNED, "abandon kept ownership");
        // Park the abandoned drone inside scoop reach of both ships.
        ensure!(f.world.with(f.system, move |s| s.set_ball_position(d4, [150.0, 0.0, 0.0], |_, _| true)).await.is_some(), "system stopped");
        wait_until(&f, "scoop positioning", move |s| s.sampled_ball(d4).and_then(|drone|
            [ship, guest_ship].into_iter().all(|id| s.sampled_ball(id).is_some_and(|b|
                crate::space::dist(drone.pos, b.pos) - f64::from(drone.radius) - f64::from(b.radius) <= DRONE_SCOOP_RANGE)).then_some(()))).await?;
        let scoop_args = [Value::list([Value::Int(d4)])];
        f.client.session.apply(vec![("charid", Value::Int(guest)), ("shipid", Value::Int(guest_ship))]);
        let error = f.call("ship", "ScoopDrone", &scoop_args).await.expect_err("foreign cargo scoop accepted");
        f.client.session.apply(vec![("charid", Value::Int(f.owner)), ("shipid", Value::Int(f.ship))]);
        ensure!(matches!(&error, crate::services::CallError::User { msg, dict } if msg.as_str() == infinity_core::messages::MSG_CUSTOM_NOTIFY
            && dict.iter().any(|(k, v)| k.as_str() == "notify" && *v == Value::unicode(infinity_core::messages::drones::CARGO_FOREIGN))), "unexpected refusal: {error:?}");
        let root = f.db.query_one("SELECT owner_id,location_id FROM game.items WHERE item_id=$1", &[&d4]).await?;
        ensure!(root.get::<_, i64>("owner_id") == OWNER_UNOWNED && root.get::<_, i64>("location_id") == f.system, "refused scoop moved the root");
        ensure!(ids(&load_items(&f.db, &f.sde, ItemQuery::Location(d4)).await?) == ids(&aboard), "refused scoop moved the cargo");
        // The owner scoops the same drone: cargo delivers with the root move.
        ensure!(f.call("ship", "ScoopDrone", &scoop_args).await.map_err(|e| anyhow!("owner scoop: {e:?}"))? == Value::None, "owner scoop refused");
        let root = f.db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&d4]).await?;
        ensure!((root.get::<_, i64>("owner_id"), root.get::<_, i64>("location_id"), root.get::<_, i32>("flag_id")) == (f.owner, f.ship, FLAG_DRONE_BAY), "scoop did not bay the root");
        ensure!(load_items(&f.db, &f.sde, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await?.iter().any(|i| aboard.iter().any(|b| b.item_id == i.item_id)), "scoop dropped cargo");
        // A cold restore loses controller and order but never durable cargo.
        ensure!(f.call("ship", "LaunchDrones", &launch_args(d4)).await.map_err(|e| anyhow!("relaunch: {e:?}"))? == Value::Dict(vec![(Value::Int(d4), Value::list([Value::Int(d4)]))]), "relaunch refused");
        wreck(&f, wreck_ty, f.owner, 5_000.0).await?;
        ensure!(command(&mut f, "CmdSalvage", &[d4], Some(Value::None)).await? == Value::Dict(vec![]), "salvage refused");
        let aboard = wait_items(&f, d4, &[], "cargo aboard", |i| !i.is_empty()).await?;
        ensure!(f.world.remove_ball(f.system, d4).await.is_some(), "drone ball missing");
        let prepared = crate::services::drones::inventory::restore_specs(&f.db, f.sde.clone(), f.system).await.map_err(|e| anyhow!("restore specs: {e:?}"))?;
        let callbacks = worker::effects(f.db.clone(), std::sync::Arc::downgrade(&f.world), f.system);
        let restored = f.world.with(f.system, move |s| {
            for prepared in prepared { s.restore_drone(prepared, callbacks.clone()); }
            s.drone(d4)
        }).await.context("system stopped")?.context("drone not restored")?;
        ensure!(restored.controller.is_none() && restored.order == DroneOrder::Idle && restored.owner == f.owner
            && !f.world.with(f.system, move |s| s.drone_carries_cargo(d4)).await.unwrap_or(false), "cold restore kept controller or markers");
        ensure!(command(&mut f, "CmdReturnBay", &[d4], None).await? == Value::Dict(vec![]), "return refused");
        wait_until(&f, "cold return", move |s| s.drone(d4).is_none().then_some(())).await?;
        let root_row = f.db.query_one("SELECT owner_id,location_id,flag_id FROM game.items WHERE item_id=$1", &[&d4]).await?;
        ensure!((root_row.get::<_, i64>("owner_id"), root_row.get::<_, i64>("location_id"), root_row.get::<_, i32>("flag_id")) == (f.owner, f.ship, FLAG_DRONE_BAY), "root not in bay");
        let held = load_items(&f.db, &f.sde, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await?;
        ensure!(aboard.iter().all(|b| held.iter().any(|i| i.item_id == b.item_id)), "committed cargo lost at restore");
        Ok(())
    }.await;
    f.cleanup().await;
    result
}
