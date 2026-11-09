//! Real Entosis lifecycle against a neutral Hub, fitted pilot and SDE module.
//! This ignored test owns its Lua host process and dedicated database domain.
mod restrictions;
use std::{sync::{Arc, Mutex}, time::Duration};
use infinity_core::{
    consts::{dogma::{ATTR_CONSUMPTION_QUANTITY, EFFECT_ENTOSIS_LINK}, FLAG_CARGO, FLAG_HANGAR},
};
use crate::{
    dao::sovereignty::{capture, sources},
    services::{combat::{Ctx, targeting_charges}, crimewatch, effects::{lifecycle, registry::{self, EffectAttribute}}, fit::{Fit, Key}, sovereignty::deploy::{self, DeployRequest}},
    space::sovereignty::links,
};
use super::fixture::{self, Fixture};

async fn blocked(f: &Fixture) -> bool {
    let ship = f.ids.ships[0];
    f.resources.world.with(f.ids.systems[1], move |s| links::blocks(s, ship)).await.unwrap()
}

async fn wait_blocked(f: &Fixture, expected: bool, seconds: u64) {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        while blocked(f).await != expected {
            assert!(f.resources.world.after(f.ids.systems[1], 100).await);
        }
    }).await.expect("Entosis actor link state deadline");
}

async fn quantity(f: &Fixture) -> i64 {
    f.resources.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&f.ids.fuel[0]]).await.unwrap().get("quantity")
}

async fn score_team(f: &Fixture, hub: i64) -> Option<i64> {
    f.resources.world.with(f.ids.systems[1], move |s| {
        s.ball(hub).and_then(|ball| ball.slim.iter().find(|(name, _)| *name == "entosis_score"))
            .and_then(|(_, value)| value.items()).and_then(|items| items.get(1)).and_then(|value| value.as_int())
    }).await.unwrap()
}
// SetBallPosition preserves motion; stop fixture ships in the same destiny boundary.
fn position_ship(s: &mut crate::space::System, id: i64, position: crate::space::V3) {
    s.set_ball_position(id, position, |_, _| true);
    s.command(id, |ball, _, _| {
        ball.stop();
        ball.vel = [0.0; 3];
        Vec::new()
    });
}

async fn wait_positions<const N: usize>(f: &Fixture, system: i64, ids: [i64; N], positions: [crate::space::V3; N], label: &str) {
    let world = &f.resources.world;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while !world.with(system, move |s| ids.iter().zip(positions).all(|(id, pos)|
            s.ball(*id).is_some_and(|ball| crate::space::dist(ball.pos, pos) < 1.0))).await.unwrap() {
            assert!(world.after(system, 100).await);
        }
    }).await;
    if result.is_err() {
        let states = world.with(system, move |s| ids.map(|id|
            s.ball(id).map(|ball| (ball.pos, ball.mode, ball.vel, ball.radius, ball.position_at(s.now_ms()))))).await;
        panic!("{label}: actor positioning deadline; ids={ids:?}, expected={positions:?}, actual (position, mode, velocity, radius, sampled position)={states:?}");
    }
}
async fn check_ranges(f: &Fixture, system: i64, pilot: usize, target: i64, entosis: bool) {
    let ids = f.ids;
    let r = &f.resources;
    let fit = Fit::load_in_space(&r.db, &r.world, system, ids.characters[pilot], ids.ships[pilot]).await.unwrap();
    let geometry = r.world.with(system, move |s| {
        let source = s.sampled_ball(ids.ships[pilot]).unwrap();
        let target = s.sampled_ball(target).unwrap();
        (source.pos, source.radius, target.pos, target.radius)
    }).await.unwrap();
    let (source_pos, source_radius, target_pos, target_radius) = geometry;
    let distance = crate::space::dist(source_pos, target_pos);
    let lock_gap = distance - f64::from(target_radius);
    let surface_gap = (lock_gap - f64::from(source_radius)).max(0.0);
    let lock_range = fit.value(Key::Ship, infinity_core::consts::dogma::ATTR_MAX_TARGET_RANGE);
    assert!(lock_gap <= lock_range, "target geometry outside effective lock range: pilot={pilot}, target={target}, source_pos={source_pos:?}, source_radius={source_radius}, target_pos={target_pos:?}, target_radius={target_radius}, lock_gap={lock_gap}, lock_range={lock_range}");
    if entosis {
        let effect = registry::resolve(&r.sde, EFFECT_ENTOSIS_LINK).unwrap();
        let range = fit.value(Key::Module(ids.modules[pilot]), effect.attribute(EffectAttribute::Range).unwrap());
        assert!(surface_gap <= range, "target geometry outside effective Entosis range: pilot={pilot}, target={target}, source_pos={source_pos:?}, source_radius={source_radius}, target_pos={target_pos:?}, target_radius={target_radius}, surface_gap={surface_gap}, range={range}");
    }
}



async fn run(f: &Fixture) {
    let r = &f.resources;
    let ids = f.ids;
    let system = ids.systems[1];
    let star = r.sde.map.celestials(system).find(|c| c.group_id == Some(i64::from(infinity_core::consts::GROUP_SUN))).unwrap();
    let staging = [star.x.unwrap() + r.sde.sovereignty.hub().deploy.fixed_star_distance_m as f64, star.y.unwrap(), star.z.unwrap()];
    f.install_pilot(0, 1, staging).await;
    f.install_pilot(2, 1, staging).await;
    let identity = f.identity(0);
    let hub = deploy::deploy(r.clone(), identity.clone(), DeployRequest { source_item_id: ids.sources[0], quantity: 1, whose_behalf: None }).await.unwrap().item_id;
    let positions = r.world.with(system, move |s| {
        let station = s.sampled_ball(ids.station).expect("real SDE station ball");
        let ship = s.sampled_ball(ids.ships[0]).unwrap();
        let capital = s.sampled_ball(ids.ships[2]).unwrap();
        let hub_ball = s.sampled_ball(hub).unwrap();
        let gap = 100.0;
        let ship_x = station.pos[0] + f64::from(station.radius) + f64::from(ship.radius) + gap;
        let capital_x = ship_x + f64::from(ship.radius) + f64::from(capital.radius) + gap;
        let hub_x = capital_x + f64::from(capital.radius) + f64::from(hub_ball.radius) + gap;
        let positions = [[ship_x, station.pos[1], station.pos[2]], [hub_x, station.pos[1], station.pos[2]], [capital_x, station.pos[1], station.pos[2]]];
        position_ship(s, ids.ships[0], positions[0]);
        s.set_ball_position(hub, positions[1], |_, _| true);
        position_ship(s, ids.ships[2], positions[2]);
        positions
    }).await.unwrap();
    wait_positions(f, system, [ids.ships[0], hub, ids.ships[2]], positions, "station and Hub").await;
    check_ranges(f, system, 0, hub, true).await;
    let original = capture::read(&r.db, hub).await.unwrap().unwrap();
    let source = sources::versions(&r.db, &[ids.characters[0]]).await.unwrap().pop().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    r.world.register_pilot(ids.characters[0], tx.clone());
    let (destiny_tx, mut destiny_rx) = tokio::sync::mpsc::unbounded_channel();
    r.world.observe(system, ids.characters[0], ids.ships[0], destiny_tx).await.expect("observe real destiny events");
    let cx = Ctx::from_identity(&identity, &r.db, &r.world, Some(system), tx.clone()).unwrap();
    targeting_charges::add_target_ctx(&cx, hub, true).await.expect("request real Hub target lock");
    tokio::time::timeout(Duration::from_secs(120), async {
        while !targeting_charges::targets_of(ids.characters[0]).contains(&hub) {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("Hub target lock deadline");
    let position = r.world.with(system, move |s| s.ball(ids.ships[0]).unwrap().pos).await.unwrap();
    let destination = [position[0] + 1.0e8, position[1], position[2]];
    let effect = registry::resolve(&r.sde, EFFECT_ENTOSIS_LINK).unwrap();
    let effect_name = r.sde.dogma.effect(EFFECT_ENTOSIS_LINK).unwrap().name.clone().unwrap();
    let cloak_type: i32 = r.db.query_one("SELECT type_id FROM game.items WHERE item_id=$1", &[&ids.cloak]).await.unwrap().get("type_id");
    let cloak_effect = r.sde.types.effects(cloak_type).iter().find_map(|entry| {
        let effect = registry::resolve(&r.sde, entry.effect_id)?;
        (effect.kind == crate::services::effects::EffectKind::Cloak).then_some(entry.effect_id)
    }).expect("fitted SDE cloak effect");
    let cloak_name = r.sde.dogma.effect(cloak_effect).unwrap().name.clone().unwrap();
    let fit = Fit::load_in_space(&r.db, &r.world, system, ids.characters[0], ids.ships[0]).await.unwrap();
    let duration = fit.value(Key::Module(ids.modules[0]), effect.attribute(EffectAttribute::Duration).unwrap());
    let fuel = fit.value(Key::Module(ids.modules[0]), ATTR_CONSUMPTION_QUANTITY).ceil() as i64;
    assert!(duration.is_finite() && duration > 0.0 && fuel > 0);
    let initial_fuel = quantity(f).await;
    assert!(initial_fuel >= fuel * 3, "fixture must provide at least three real fuel cycles");

    // An unavailable fuel stack must not create a paid interval or start FX.
    r.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&ids.fuel[0], &FLAG_HANGAR]).await.unwrap();
    lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.modules[0], &effect_name, Some(hub), 10, |_| Ok(())).await.expect("activation request");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (name, args) = rx.recv().await.expect("effect notification channel");
            assert_ne!(name, "OnGodmaShipEffect", "unpaid activation emitted effect FX");
            if name == "OnEffectMessage" {
                assert_eq!(args, vec![marshal::Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY), marshal::Value::Dict(vec![(marshal::Value::str("notify"), marshal::Value::unicode(infinity_core::messages::effects::INSUFFICIENT_FUEL))])]);
                break;
            }
        }
    }).await.expect("fuel refusal notification deadline");
    assert!(!blocked(f).await);
    assert!(crimewatch::weapons_timer(ids.characters[0]).is_none());
    r.db.execute("UPDATE game.items SET flag_id=$2 WHERE item_id=$1", &[&ids.fuel[0], &FLAG_CARGO]).await.unwrap();
    // The failure notification precedes actor cleanup; wait until a new activation can own the module.
    tokio::time::timeout(Duration::from_secs(30), async {
        while crate::services::combat::active_modules().contains(&ids.modules[0]) {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("unpaid activation cleanup");
    while let Ok((name, _)) = rx.try_recv() {
        assert_ne!(name, "OnGodmaShipEffect", "unpaid activation emitted effect FX");
    }
    assert!(r.world.after(system, 100).await);
    while let Ok(batch) = destiny_rx.try_recv() {
        for event in batch {
            let call = event.items().and_then(|envelope| envelope.get(1)).and_then(|call| call.items());
            if let Some(call) = call {
                if call.first() == Some(&marshal::Value::str("OnSpecialFX")) {
                    let args = call.get(1).and_then(|args| args.items()).unwrap();
                    assert!(!(args.get(1).and_then(|v| v.as_int()) == Some(ids.modules[0]) && args.get(7).and_then(|v| v.as_int()) == Some(1)), "unpaid activation emitted start FX");
                }
            }
        }
    }
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let (admitted_tx, admitted_rx) = tokio::sync::oneshot::channel();
    let dock_db = r.db.clone();
    let dock_world = r.world.clone();
    let mut dock_task = tokio::spawn(async move {
        let mut observer = restrictions::DockBarrier { world: dock_world.clone(), client: ids.characters[0], accepted: Some(accepted_tx), admitted: Some(admitted_rx) };
        crate::services::ship::dock(&identity, &dock_db, &dock_world, Some(system), ids.station, Some(&mut observer)).await
    });
    assert_eq!(tokio::time::timeout(Duration::from_secs(30), accepted_rx).await.expect("dock acceptance deadline").expect("dock accepted"), ids.station);
    lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.modules[0], &effect_name, Some(hub), 10, |_| Ok(())).await.expect("paid activation request");
    wait_blocked(f, true, 30).await;
    admitted_tx.send(()).expect("dock barrier admission");
    let dock_result = tokio::time::timeout(Duration::from_secs(30), &mut dock_task).await.expect("dock post-wait deadline").expect("dock task");
    assert!(matches!(dock_result, Err(crate::services::CallError::User { dict, .. }) if dict.iter().any(|(key, value)|
        key == "notify" && *value == marshal::Value::unicode(infinity_core::messages::effects::ENTOSIS_ACTION_BLOCKED))), "dock accepted before link must be refused at the actual post-wait gate");
    let location = crate::dao::characters::ship::current(&r.db, ids.characters[0]).await.unwrap().unwrap();
    assert_eq!((location.station, location.system, location.ship), (None, Some(system), Some(ids.ships[0])));
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (name, _) = rx.recv().await.expect("effect notification channel");
            if name == "OnGodmaShipEffect" { break; }
        }
    }).await.expect("paid activation effect notification");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let batch = destiny_rx.recv().await.expect("destiny observer channel");
            if batch.iter().any(|event| {
                let Some(call) = event.items().and_then(|envelope| envelope.get(1)).and_then(|call| call.items()) else { return false };
                if call.first() != Some(&marshal::Value::str("OnSpecialFX")) { return false; }
                let Some(args) = call.get(1).and_then(|args| args.items()) else { return false };
                args.get(1).and_then(|v| v.as_int()) == Some(ids.modules[0]) && args.get(7).and_then(|v| v.as_int()) == Some(1)
            }) { break; }
        }
    }).await.expect("paid activation start FX");
    assert!(crate::services::ship::warp_to(&identity, &r.db, &r.world, Some(system), crate::space::system::warp::WarpDestination::Ordinary { point: destination, object_radius: None, min_range: 0.0 }, None).await.is_err(), "admitted link must deny warp");
    let cloak_refusal = lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.cloak, &cloak_name, None, 1, |_| Ok(())).await;
    assert!(matches!(cloak_refusal, Err(crate::services::CallError::User { dict, .. }) if dict.iter().any(|(key, value)|
        key == "notify" && *value == marshal::Value::unicode(infinity_core::messages::effects::ENTOSIS_ACTION_BLOCKED))), "actual cloak must be refused by the Entosis gate");
    assert_eq!(quantity(f).await, initial_fuel - fuel);
    assert!(crimewatch::weapons_timer(ids.characters[0]).is_some());
    let capital = fit.value(Key::Ship, infinity_core::consts::dogma::ATTR_IS_CAPITAL_SIZE);
    let assistance_blocked = r.world.with(system, move |s| links::assistance_blocked(s, ids.ships[0])).await.unwrap();
    assert_eq!(assistance_blocked, capital.is_finite() && capital > 0.0, "assistance must use the actual fitted hull attribute");
    restrictions::guarded_capacitor(f, 0, 2, false).await;
    assert_eq!(score_team(f, hub).await, None, "first paid cycle must only warm up");
    assert_eq!(capture::read(&r.db, hub).await.unwrap().unwrap().score, original.score);
    let deadline = Duration::from_millis(duration.ceil() as u64).saturating_add(Duration::from_secs(30));
    tokio::time::timeout(deadline, async {
        loop {
            if quantity(f).await == initial_fuel - 2 * fuel && score_team(f, hub).await == Some(ids.alliances[0]) { break; }
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("second paid scoring cycle deadline");
    let checkpoint_deadline = Duration::from_millis(original.checkpoint_interval_ms.max(1) as u64).saturating_add(Duration::from_secs(30));
    tokio::time::timeout(checkpoint_deadline, async {
        loop {
            let checkpoint = capture::read(&r.db, hub).await.unwrap().unwrap();
            if checkpoint.score > original.score && checkpoint.scoring_team == Some(ids.alliances[0]) { break; }
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("real paid scoring checkpoint deadline");
    assert_eq!(sources::versions(&r.db, &[ids.characters[0]]).await.unwrap().pop().unwrap(), source);
    lifecycle::deactivate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.modules[0], &effect_name, |_| Ok(())).await.expect("request stop");
    assert!(blocked(f).await, "stop request must not retire a paid link");
    assert!(crate::services::ship::warp_to(&identity, &r.db, &r.world, Some(system), crate::space::system::warp::WarpDestination::Ordinary { point: destination, object_radius: None, min_range: 0.0 }, None).await.is_err(), "requested stop must still deny warp");
    tokio::time::timeout(deadline, async {
        while blocked(f).await { assert!(r.world.after(system, 100).await); }
    }).await.expect("actual cycle cleanup deadline");

    // Removing the real lock retires the next admitted link.
    while rx.try_recv().is_ok() {}
    lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.modules[0], &effect_name, Some(hub), 10, |_| Ok(())).await.expect("restart link");
    wait_blocked(f, true, 30).await;
    targeting_charges::clear_locks_on(&r.world, system, ids.ships[0], ids.characters[0]);
    wait_blocked(f, false, 30).await;
    tokio::time::timeout(deadline, async {
        loop {
            let (name, args) = rx.recv().await.expect("effect notification channel");
            if name == "OnGodmaShipEffect" && args.first().and_then(|v| v.as_int()) == Some(ids.modules[0])
                && args.get(3).and_then(|v| v.as_int()) == Some(0) { break; }
        }
    }).await.expect("lock-loss lifecycle cleanup");
    targeting_charges::add_target_ctx(&cx, hub, true).await.expect("reacquire Hub target");
    tokio::time::timeout(Duration::from_secs(120), async {
        while !targeting_charges::targets_of(ids.characters[0]).contains(&hub) {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("Hub relock deadline");
    lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.modules[0], &effect_name, Some(hub), 10, |_| Ok(())).await.expect("restart after lock loss");
    wait_blocked(f, true, 30).await;
    r.world.remove_ball(system, ids.ships[0]).await;
    wait_blocked(f, false, 30).await;
    targeting_charges::clear_locks_on(&r.world, system, ids.ships[0], ids.characters[0]);
    // Stage a second real deployment in the connected gate's system.
    let system = ids.systems[0];
    let star = r.sde.map.celestials(system).find(|c| c.group_id == Some(i64::from(infinity_core::consts::GROUP_SUN))).unwrap();
    let staging = [star.x.unwrap() + r.sde.sovereignty.hub().deploy.fixed_star_distance_m as f64, star.y.unwrap(), star.z.unwrap()];
    f.install_pilot(1, 0, staging).await;
    let hub = deploy::deploy(r.clone(), f.identity(1), DeployRequest { source_item_id: ids.sources[1], quantity: 1, whose_behalf: None }).await.unwrap().item_id;
    f.install_pilot(0, 0, staging).await;
    f.install_pilot(2, 0, staging).await;
    let identity = f.identity(0);
    let gate_positions = r.world.with(system, move |s| {
        let gate = s.sampled_ball(ids.gate).expect("real SDE stargate ball");
        let ship = s.sampled_ball(ids.ships[0]).unwrap();
        let capital = s.sampled_ball(ids.ships[2]).unwrap();
        let hub_ball = s.sampled_ball(hub).unwrap();
        let gap = 100.0;
        let capital_x = gate.pos[0] + f64::from(gate.radius) + f64::from(capital.radius) + gap;
        let ship_x = capital_x + f64::from(capital.radius) + f64::from(ship.radius) + gap;
        let hub_x = ship_x + f64::from(ship.radius) + f64::from(hub_ball.radius) + gap;
        let positions = [[ship_x, gate.pos[1], gate.pos[2]], [hub_x, gate.pos[1], gate.pos[2]], [capital_x, gate.pos[1], gate.pos[2]]];
        position_ship(s, ids.ships[0], positions[0]);
        s.set_ball_position(hub, positions[1], |_, _| true);
        position_ship(s, ids.ships[2], positions[2]);
        positions
    }).await.unwrap();
    wait_positions(f, system, [ids.ships[0], hub, ids.ships[2]], gate_positions, "gate and Hub").await;
    check_ranges(f, system, 2, hub, true).await;
    check_ranges(f, system, 0, ids.ships[2], false).await;
    let capital_identity = f.identity(2);
    let (capital_tx, mut capital_rx) = tokio::sync::mpsc::unbounded_channel();
    r.world.register_pilot(ids.characters[2], capital_tx.clone());
    let capital_cx = Ctx::from_identity(&capital_identity, &r.db, &r.world, Some(system), capital_tx.clone()).unwrap();
    let capital_fit = Fit::load_in_space(&r.db, &r.world, system, ids.characters[2], ids.ships[2]).await.unwrap();
    assert!(capital_fit.value(Key::Ship, infinity_core::consts::dogma::ATTR_IS_CAPITAL_SIZE) > 0.0);
    let capital_duration = capital_fit.value(Key::Module(ids.modules[2]), effect.attribute(EffectAttribute::Duration).unwrap());
    assert!(capital_duration.is_finite() && capital_duration > duration, "capital passive SDE modifier must lengthen the effective module duration");
    let hull = capital_fit.source_type(Key::Ship).expect("capital hull type");
    let passive = r.sde.dogma.effects_by_name("entosisDurationMultiply").next().expect("capital Entosis duration modifier");
    assert!(r.sde.types.effects(hull).iter().any(|entry| entry.effect_id == passive.effect_id));
    targeting_charges::add_target_ctx(&capital_cx, hub, true).await.expect("capital target lock request");
    tokio::time::timeout(Duration::from_secs(120), async {
        while !targeting_charges::targets_of(ids.characters[2]).contains(&hub) {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("capital Hub lock deadline");
    let (projection_attrs, projection_baseline) = restrictions::start_projection(f, tx.clone()).await;
    let projection_active = restrictions::projection_values(f, &projection_attrs).await;
    assert_ne!(projection_active, projection_baseline, "the real assistance projection must modify the capital fit");
    let capital_fit = Fit::load_in_space(&r.db, &r.world, system, ids.characters[2], ids.ships[2]).await.unwrap();
    let capital_duration = capital_fit.value(Key::Module(ids.modules[2]), effect.attribute(EffectAttribute::Duration).unwrap());
    let mut gate_task = restrictions::start_gate(f).await;
    lifecycle::activate(&capital_identity, &r.db, &r.world, &r.sde, Some(system), capital_tx, ids.account, ids.modules[2], &effect_name, Some(hub), 10, |_| Ok(())).await.expect("capital Entosis activation");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (name, args) = capital_rx.recv().await.expect("capital effect notification channel");
            if name == "OnGodmaShipEffect" && args.first().and_then(|v| v.as_int()) == Some(ids.modules[2]) && args.get(3).and_then(|v| v.as_int()) == Some(1) {
                assert_eq!(args.get(7).and_then(|v| v.as_float()), Some(capital_duration), "cycle duration must be the real effective Fit value");
                break;
            }
        }
    }).await.expect("capital paid cycle notification");
    assert!(r.world.with(system, move |s| links::blocks(s, ids.ships[2]) && links::assistance_blocked(s, ids.ships[2])).await.unwrap(), "actual capital link must block assistance");
    let (gate_result, gate_session) = tokio::time::timeout(Duration::from_secs(30), &mut gate_task).await.expect("gate post-wait deadline").expect("gate task");
    assert!(matches!(gate_result, Err(crate::services::CallError::User { dict, .. }) if dict.iter().any(|(key, value)|
        key == "notify" && *value == marshal::Value::unicode(infinity_core::messages::effects::ENTOSIS_ACTION_BLOCKED))), "jump accepted before link must be refused at the native post-wait gate");
    assert_eq!(gate_session, Some(system));
    let capital_location = crate::dao::characters::ship::current(&r.db, ids.characters[2]).await.unwrap().unwrap();
    assert_eq!((capital_location.station, capital_location.system, capital_location.ship), (None, Some(system), Some(ids.ships[2])));
    assert!(r.world.request_entry(ids.destination_system, ids.characters[2]).await.is_ok(), "refused source transition must not strand its destination entry");
    assert!(r.world.request_entry(ids.destination_system, ids.characters[1]).await.is_ok(), "refused source transition must not strand another pilot in the destination queue");
    assert!(!restrictions::projection_visible(f).await, "an existing remote assistance projection must be suppressed");
    assert_eq!(restrictions::projection_values(f, &projection_attrs).await, projection_baseline, "suppressed assistance must be removed from the effective fit");
    restrictions::guarded_capacitor(f, 2, 0, true).await;
    restrictions::self_capacitor(f, 2, 0).await;
    let range = capital_fit.value(Key::Module(ids.modules[2]), effect.attribute(EffectAttribute::Range).unwrap());
    r.world.with(system, move |s| {
        let target = s.sampled_ball(hub).unwrap();
        let source = s.sampled_ball(ids.ships[2]).unwrap();
        let outside = [target.pos[0] + range + f64::from(target.radius) + f64::from(source.radius) + 1_000.0, target.pos[1], target.pos[2]];
        position_ship(s, ids.ships[2], outside);
    }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while r.world.with(system, move |s| links::blocks(s, ids.ships[2])).await.unwrap() {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("out-of-range link cleanup");
    assert!(!r.world.with(system, move |s| links::assistance_blocked(s, ids.ships[2])).await.unwrap());
    assert!(restrictions::projection_visible(f).await, "the existing projection must reappear after actual link retirement");
    assert_eq!(restrictions::projection_values(f, &projection_attrs).await, projection_active, "the effective fit must restore the existing projection");
    r.world.remove_ball(system, ids.ships[2]).await;
    r.world.with(system, move |s| position_ship(s, ids.ships[0], destination)).await.unwrap();
    wait_positions(f, system, [ids.ships[0]], [destination], "cloak clearance").await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while r.world.with(system, move |s| s.cloak_obstructed(ids.ships[0])).await.unwrap() {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("clear cloak geometry");
    lifecycle::activate(&identity, &r.db, &r.world, &r.sde, Some(system), tx.clone(), ids.account, ids.cloak, &cloak_name, None, 1, |_| Ok(())).await.expect("cloak allowed after link retirement");
    tokio::time::timeout(Duration::from_secs(30), async {
        while !r.world.with(system, move |s| s.ball(ids.ships[0]).is_some_and(|b| b.cloak != 0)).await.unwrap() {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("actual cloak admission");
    lifecycle::deactivate(&identity, &r.db, &r.world, &r.sde, Some(system), tx, ids.account, ids.cloak, &cloak_name, |_| Ok(())).await.expect("stop cloak");
    tokio::time::timeout(Duration::from_secs(30), async {
        while r.world.with(system, move |s| s.has_module_cloak(ids.ships[0])).await.unwrap() {
            assert!(r.world.after(system, 100).await);
        }
    }).await.expect("cloak cleanup");
    assert!(crate::services::ship::warp_to(&identity, &r.db, &r.world, Some(system), crate::space::system::warp::WarpDestination::Ordinary { point: destination, object_radius: None, min_range: 0.0 }, None).await.is_ok(), "warp must be admitted after real link cleanup");
}

#[test]
#[ignore = "requires a dedicated initialized INFINITY_TEST_DATABASE_URL and real SDE; run this exact test in its own process"]
fn entosis_paid_lifecycle_integration() {
    crate::tests::init_config();
    let state = Arc::new(Mutex::new(fixture::Recorded::default()));
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let shared = state.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(async {
        let mut task = tokio::spawn(async move { let f = Fixture::new(shared).await; run(&f).await; });
        let result = tokio::time::timeout(Duration::from_secs(450), &mut task).await;
        if result.is_err() { task.abort(); let _ = task.await; }
        let world = state.lock().unwrap().world.clone();
        if let Some(world) = world { crate::space::sovereignty::close(&world); }
        let _fence = crate::services::market::lock().await;
        match result {
            Ok(Ok(())) => {},
            Ok(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Ok(Err(error)) => panic!("integration task cancelled: {error}"),
            Err(_) => panic!("Entosis integration scenario timed out"),
        }
    })));
    drop(runtime);
    let ids = state.lock().unwrap().ids;
    let cleanup_runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleanup_runtime.block_on(fixture::cleanup(ids))));
    if let Err(error) = cleanup {
        eprintln!("Entosis integration fixture cleanup failed: {error:?}");
        if let Err(original) = result { std::panic::resume_unwind(original); }
        std::panic::resume_unwind(error);
    }
    if let Err(error) = result { std::panic::resume_unwind(error); }
}
