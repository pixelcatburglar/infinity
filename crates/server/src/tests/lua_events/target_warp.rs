//! Real Targeted and WarpArrived producers observed through the shared Lua host.

mod fleet;

use std::{future::Future, sync::Arc, time::Duration};
use crate::{consts::{CATEGORY_SHIP, dogma::{ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_SIGNATURE_RADIUS}},
    services::{combat::{self, targeting_charges}, identity::Identity},
    space::{Ball, System, WarpState, MODE_STOP, HOSTILE_TO_ALL, NpcBrain, NpcConfig}};
use serde_json::json;

const SYSTEM: i64 = 1;

fn ball(id: i64, type_id: i32, pilot: Option<i64>, x: f64) -> Ball {
    Ball { id, type_id, owner_id: pilot.unwrap_or(0), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: pilot.is_some(),
        warp: WarpState::default(), warp_factor: 3000, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn assert_new(ctx: &super::Context, cursor: &mut usize, expected: &[serde_json::Value]) {
    let events = ctx.events().await;
    assert_eq!(&events[*cursor..], expected);
    *cursor = events.len();
}

async fn warp(ctx: &super::Context, cursor: &mut usize) {
    use crate::space::system::tests::events::tick_with_collisions;
    for collisions in [false, true] {
        for callback in [false, true] {
            let mut s = System::new(SYSTEM, Arc::default(), 0);
            let mut b = ball(1, 1, Some(10), 0.0);
            b.vel = [100.0, 0.0, 0.0];
            s.insert(b);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            s.command(1, |b, _, _| { b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
            if callback { s.on_warp_exit(1, move |_| { tx.send(()).unwrap(); }); }
            for tick in 1..=100 {
                tick_with_collisions(&mut s, tick * 1000, collisions);
                if s.ball(1).unwrap().mode == MODE_STOP { break; }
            }
            assert_eq!(s.ball(1).unwrap().mode, MODE_STOP);
            assert_eq!(rx.try_recv().is_ok(), callback);
            tick_with_collisions(&mut s, 101_000, collisions);
            assert_new(ctx, cursor, &[json!({"kind":"WarpArrived", "char_id":10, "system_id":SYSTEM})]).await;
        }
    }
    let mut s = System::new(SYSTEM, Arc::default(), 0);
    s.insert(ball(1, 1, Some(10), 0.0));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    s.command(1, |b, _, _| { b.warp_to([1000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
    s.on_warp_exit(1, move |_| { tx.send(()).unwrap(); });
    s.tick(1000);
    assert!(rx.try_recv().is_ok());
    assert_new(ctx, cursor, &[]).await;
    s.command(1, |b, _, _| { b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
    s.tick(2000);
    s.take(1);
    s.tick(3000);
    assert_new(ctx, cursor, &[]).await;
    let mut unpiloted = ball(2, 1, None, 0.0);
    unpiloted.vel = [100.0, 0.0, 0.0];
    s.insert(unpiloted);
    s.command(2, |b, _, _| { b.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000); Vec::new() });
    for tick in 4..=103 { s.tick(tick * 1000); if s.ball(2).unwrap().mode == MODE_STOP { break; } }
    assert_eq!(s.ball(2).unwrap().mode, MODE_STOP);
    assert_new(ctx, cursor, &[]).await;
}

async fn npc(ctx: &super::Context, cursor: &mut usize) {
    let mut s = System::new(SYSTEM, Arc::default(), 0);
    s.insert(ball(1000, 1, None, 0.0));
    s.insert(ball(1, 1, Some(10), 50.0));
    let brain = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_| Box::pin(async {})));
    assert!(s.add_npc(1000, brain));
    s.tick(0);
    assert_new(ctx, cursor, &[]).await;
    for tick in 1..=60 { s.tick(tick * 1000); }
    assert_new(ctx, cursor, &[json!({"kind":"Targeted", "char_id":10, "by_ball_id":1000, "system_id":SYSTEM})]).await;
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(10, tx);
    let mut s = System::new(SYSTEM, registry, 0);
    s.insert(ball(1000, 1, None, 0.0));
    let mut drone = ball(2, 1, None, 50.0);
    drone.owner_id = 10;
    let spec = crate::space::drone::DroneSpec { type_id: 1, bandwidth: 1.0, scan_resolution: 1.0,
        control_range: 100.0, max_target_range: 100.0, optimal_range: 50.0, orbit_range: 20.0,
        duration_ms: 1000.0, effect_id: None };
    s.restore_drone(drone, spec, Arc::new(|_| Box::pin(async {})));
    let brain = NpcBrain::new(Some(10), NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_| Box::pin(async {})));
    assert!(s.add_npc(1000, brain));
    for tick in 0..=60 { s.tick(tick * 1000); }
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|(name, args)|
        name == "OnTarget" && args[0].as_bytes() == Some(b"otheradd".as_slice())));
    assert_new(ctx, cursor, &[]).await;
    let mut s = System::new(SYSTEM, Arc::default(), 0);
    s.insert(ball(1001, 1, None, 0.0));
    s.insert(ball(1, 1, Some(10), 50.0));
    let brain = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_| Box::pin(async {})));
    assert!(s.add_npc(1001, brain));
    s.tick(1000);
    s.take(1);
    for tick in 2..=60 { s.tick(tick * 1000); }
    assert_new(ctx, cursor, &[]).await;
}

pub(super) async fn run(ctx: &super::Context) {
    let mut cursor = ctx.events().await.len();
    warp(ctx, &mut cursor).await;
    npc(ctx, &mut cursor).await;
    player(ctx, &mut cursor).await;
    fleet::run(ctx).await;
}

async fn player(ctx: &super::Context, cursor: &mut usize) {
    let db = &ctx.resources.db;
    let world = &ctx.resources.world;
    let ty = ctx.resources.sde.types.iter().filter(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_MAX_TARGET_RANGE).unwrap_or(0.0) > 0.0
        && t.attribute(ATTR_SIGNATURE_RADIUS).unwrap_or(0.0) > 0.0)
        .max_by(|a, b| a.attribute(ATTR_SCAN_RESOLUTION).unwrap_or(0.0).total_cmp(&b.attribute(ATTR_SCAN_RESOLUTION).unwrap_or(0.0)))
        .expect("ship type").type_id;
    let (account, character, ship): (i64, i64, i64) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('event-lock-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'event-lock-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = tx.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES ($1, $2, $3, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&ty, &character, &SYSTEM]).await?.get("item_id");
        Ok::<_, crate::db::Error>((account, character, ship))
    }), |f, ()| async move { Ok::<_, crate::db::Error>(f) }).await.unwrap();
    let target = -ship;
    let pilot = -character;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (target_tx, mut target_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(pilot, target_tx);
    let identity = Identity { char_id: character, ship_id: Some(ship), corp_id: 1, station_id: None, system_id: Some(SYSTEM) };
    let cx = combat::Ctx::from_identity(&identity, db, world, Some(SYSTEM), tx).unwrap();
    let mut work = std::pin::pin!(async {
        world.add_ship(SYSTEM, ball(ship, ty, Some(character), 0.0)).await.unwrap();
        world.add_ship(SYSTEM, ball(target, ty, Some(pilot), 0.0)).await.unwrap();
        let started = std::time::Instant::now();
        targeting_charges::add_target_ctx(&cx, target, true).await.ok().expect("lock starts");
        assert_new(ctx, cursor, &[]).await;
        let (_, args) = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
        assert_new(ctx, cursor, &[json!({"kind":"Targeted", "char_id":pilot, "by_ball_id":ship, "system_id":SYSTEM})]).await;
        let wait_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64 + 1000;
        combat::reset_pilot(world, character, None);
        targeting_charges::add_target_ctx(&cx, target, false).await.ok().expect("announced lock starts");
        let (_, args) = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
        let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"otheradd".as_slice()));
        assert_new(ctx, cursor, &[json!({"kind":"Targeted", "char_id":pilot, "by_ball_id":ship, "system_id":SYSTEM})]).await;
        combat::reset_pilot(world, character, None);
        let (_, args) = tokio::time::timeout(Duration::from_secs(2), target_rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
        targeting_charges::add_target_ctx(&cx, target, false).await.ok().expect("second lock starts");
        world.remove_ball(SYSTEM, target).await;
        let (_, args) = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"lost".as_slice()));
        assert_new(ctx, cursor, &[]).await;
        let mut drone = ball(target, ty, None, 0.0);
        drone.owner_id = pilot;
        world.add_ship(SYSTEM, drone).await.unwrap();
        targeting_charges::add_target_ctx(&cx, target, true).await.ok().expect("unpiloted target lock starts");
        let (_, args) = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.unwrap().unwrap();
        assert_eq!(args[0].as_bytes(), Some(b"add".as_slice()));
        assert_new(ctx, cursor, &[]).await;
        combat::reset_pilot(world, character, None);
        world.remove_ball(SYSTEM, target).await;
        world.add_ship(SYSTEM, ball(target, ty, Some(pilot), 0.0)).await.unwrap();
        targeting_charges::add_target_ctx(&cx, target, true).await.ok().expect("cancelled lock starts");
        combat::reset_pilot(world, character, None);
        assert!(world.after(SYSTEM, wait_ms).await);
        assert_new(ctx, cursor, &[]).await;
        targeting_charges::add_target_ctx(&cx, target, true).await.ok().expect("jammed lock starts");
        world.with(SYSTEM, move |s| combat::jamming::apply(s, target, target, ship, wait_ms as f64)).await.unwrap();
        assert!(world.after(SYSTEM, wait_ms).await);
        assert_new(ctx, cursor, &[]).await;
    });
    let result = std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.as_mut().poll(cx))) {
        Ok(v) => v.map(Ok), Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await;
    combat::reset_pilot(world, character, Some((SYSTEM, ship)));
    world.remove_ball(SYSTEM, ship).await;
    world.remove_ball(SYSTEM, target).await;
    world.unregister_pilot(pilot);
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id = $1", &[&character]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok::<_, crate::db::Error>(()) }).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
