//! Damage producer acceptance through the shared Lua observer.
use std::{future::Future, sync::Arc, time::Duration};
use serde_json::json;
use crate::{consts::{CATEGORY_SHIP, CATEGORY_CHARGE, dogma::{ATTR_SHIELD_CAPACITY, ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_HP, ATTR_EMP_FIELD_RANGE, ATTR_AOE_CLOUD_SIZE, EFFECT_NPC_BEHAVIOR_SMARTBOMB, ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE}},
    services::{combat::{self, groups_health}, effects::{aoe, registry, weapons::{self, Missile}}},
    space::{Ball, WarpState, MODE_STOP, NpcAction, NpcBrain, NpcConfig, HOSTILE_TO_ALL}};

const SYSTEM: i64 = 1;

fn ball(id: i64, type_id: i32, pilot: Option<i64>) -> Ball {
    Ball { id, type_id, owner_id: pilot.unwrap_or(0), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(100.0), mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: false, interactive: pilot.is_some(),
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn expect_events(ctx: &super::Context, cursor: &mut usize, expected: &[serde_json::Value]) {
    let events = ctx.events().await;
    assert_eq!(&events[*cursor..], expected);
    *cursor = events.len();
}

pub(super) async fn run(ctx: &super::Context) {
    let db = &ctx.resources.db;
    let world = &ctx.resources.world;
    let sde = &ctx.resources.sde;
    let damage_attrs = [ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE];
    let npc = sde.types.iter().filter(|t| t.effects().iter().any(|e| e.effect_id == EFFECT_NPC_BEHAVIOR_SMARTBOMB)
        && t.attribute(ATTR_EMP_FIELD_RANGE).is_some_and(|v| v > 1.0)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 100.0)
        && damage_attrs.into_iter().any(|a| t.attribute(a).is_some_and(|v| v > 0.0)))
        .min_by(|a, b| damage_attrs.into_iter().map(|id| a.attribute(id).unwrap_or(0.0).max(0.0)).sum::<f64>()
            .total_cmp(&damage_attrs.into_iter().map(|id| b.attribute(id).unwrap_or(0.0).max(0.0)).sum::<f64>()))
        .expect("NPC smartbomb source");
    let npc_type = npc.type_id;
    let npc_damage = damage_attrs.into_iter().map(|id| npc.attribute(id).unwrap_or(0.0).max(0.0)).sum::<f64>();
    let hull = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 100.0 + npc_damage * 10.0)
        && t.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).is_some_and(|v| v > 0.0))
        .max_by(|a, b| a.attribute(ATTR_SHIELD_CAPACITY).unwrap().total_cmp(&b.attribute(ATTR_SHIELD_CAPACITY).unwrap()))
        .expect("damageable ship hull").type_id;
    let charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)
        && t.attribute(ATTR_AOE_CLOUD_SIZE).is_some_and(|v| v > 0.0)).expect("missile charge type").type_id;
    let mut cursor = ctx.events().await.len();
    let (account, pilot, ship, unpiloted): (i64, i64, i64, i64) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('event-damage-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let pilot = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'event-damage-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = crate::dao::items::creation::positioned(tx, hull, pilot, SYSTEM, [0.0; 3]).await?;
        let unpiloted = crate::dao::items::creation::positioned(tx, hull, pilot, SYSTEM, [0.0; 3]).await?;
        Ok::<_, crate::db::Error>((account, pilot, ship, unpiloted))
    }), |result, ()| async move { Ok::<_, crate::db::Error>(result) }).await.unwrap();
    let source = -ship;
    let mut work = std::pin::pin!(async {
        world.add_ship(SYSTEM, ball(ship, hull, Some(pilot))).await.unwrap();
        let mut source_ball = ball(source, npc_type, None);
        source_ball.pos = [1.0, 0.0, 0.0];
        source_ball.old_pos = source_ball.pos;
        world.add_ship(SYSTEM, source_ball).await.unwrap();
        let mut empty = ball(unpiloted, hull, None);
        empty.owner_id = pilot;
        world.with(SYSTEM, move |s| s.add_persistent_ship(empty)).await.unwrap();
        let attacker = world.ball(SYSTEM, source).await.unwrap();
        let target = world.ball(SYSTEM, ship).await.unwrap();
        let event = json!({"kind":"Damaged","char_id":pilot,"ship_id":ship,"attacker_ball_id":source,"system_id":SYSTEM});
        weapons::turret_shot(db, world, SYSTEM, &attacker, &target, None, npc_type, (1.0, 1.0, 1.0, 1.0), [1.0, 0.0, 0.0, 0.0], None).await.ok().unwrap();
        world.with(SYSTEM, |_| ()).await.unwrap();
        expect_events(ctx, &mut cursor, &[event.clone()]).await;
        world.with(SYSTEM, |_| ()).await.unwrap();
        assert!(world.ball(SYSTEM, ship).await.unwrap().health[0] < 1.0);
        weapons::turret_shot(db, world, SYSTEM, &attacker, &target, None, npc_type, (1.0, 1.0, 1.0, 1.0), [0.0; 4], None).await.ok().unwrap();
        world.with(SYSTEM, |_| ()).await.unwrap();
        expect_events(ctx, &mut cursor, &[]).await;
        weapons::turret_shot(db, world, SYSTEM, &attacker, &target, None, npc_type, (-1.0, 1.0, 1.0, 1.0), [1.0; 4], None).await.ok().unwrap();
        expect_events(ctx, &mut cursor, &[]).await;
        assert!(groups_health::damage(db, world, SYSTEM, unpiloted, [1.0, 0.0, 0.0, 0.0], None, Some(source)).await.ok().unwrap() > 0.0);
        groups_health::damage(db, world, SYSTEM, source, [1.0, 0.0, 0.0, 0.0], None, Some(ship)).await.ok().unwrap();
        expect_events(ctx, &mut cursor, &[]).await;
        world.remove_ball(SYSTEM, unpiloted).await;
        combat::forget_ship(unpiloted);
        let missile = Missile { charge, raw: [1.0, 0.0, 0.0, 0.0], aoe_cloud: 1.0, aoe_velocity: 100.0,
            drf: 1.0, velocity: 1000.0, flight_range: 1000.0, weapon: npc_type, attacker: None };
        weapons::launch_missile(db.clone(), world.clone(), SYSTEM, attacker.clone(), target.clone(), 1, None, missile, None).await.ok().unwrap();
        world.remove_ball(SYSTEM, source).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if ctx.events().await.len() > cursor { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("missile damage event");
        expect_events(ctx, &mut cursor, &[event.clone()]).await;
        world.add_ship(SYSTEM, attacker.clone()).await.unwrap();
        let (shots, mut rx) = tokio::sync::mpsc::unbounded_channel();
        world.with(SYSTEM, move |s| {
            let mut brain = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig { fly_range: 1.0, aggro_range: 1000.0,
                rate_of_fire_ms: 1e9, scan_resolution: 17_163.0, repairs: (0.0, 0.0, 1000.0) }, Arc::new(move |action| {
                if let NpcAction::Shot { generation, .. } = action { let _ = shots.send(generation); }
                Box::pin(async {})
            }));
            brain.target = Some((ship, 1, 0)); brain.announced = true;
            s.add_npc(source, brain);
        }).await.unwrap();
        let generation = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
        let effect = registry::resolve(sde, EFFECT_NPC_BEHAVIOR_SMARTBOMB).unwrap();
        aoe::apply_npc(db, world, SYSTEM, 0, &attacker, &target, &effect, 1.0, generation).await.ok().unwrap();
        world.with(SYSTEM, |_| ()).await.unwrap();
        expect_events(ctx, &mut cursor, &[event.clone()]).await;
        groups_health::damage(db, world, SYSTEM, ship, [1.0, 0.0, 0.0, 0.0], None, None).await.ok().unwrap();
        world.with(SYSTEM, |_| ()).await.unwrap();
        expect_events(ctx, &mut cursor, &[]).await;
        assert!(crate::dao::items::dogma::set_override(db, ship, ATTR_SHIELD_EM_DAMAGE_RESONANCE, 0.0).await.unwrap());
        assert_eq!(groups_health::damage(db, world, SYSTEM, ship, [1.0, 0.0, 0.0, 0.0], None, Some(source)).await.ok().unwrap(), 0.0);
        world.with(SYSTEM, |_| ()).await.unwrap();
        expect_events(ctx, &mut cursor, &[]).await;
        let resonance = sde.types.attribute(hull, ATTR_SHIELD_EM_DAMAGE_RESONANCE).unwrap();
        assert!(crate::dao::items::dogma::set_override(db, ship, ATTR_SHIELD_EM_DAMAGE_RESONANCE, resonance).await.unwrap());
        groups_health::damage(db, world, SYSTEM, ship, [1e30; 4], None, Some(source)).await.ok().unwrap();
        assert!(world.ball(SYSTEM, ship).await.is_none());
        expect_events(ctx, &mut cursor, &[event]).await;
        groups_health::damage(db, world, SYSTEM, ship, [1.0; 4], None, Some(source)).await.ok().unwrap();
        expect_events(ctx, &mut cursor, &[]).await;
    });
    let result = std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.as_mut().poll(cx))) {
        Ok(result) => result.map(Ok), Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await;
    world.remove_ball(SYSTEM, ship).await;
    world.remove_ball(SYSTEM, source).await;
    world.remove_ball(SYSTEM, unpiloted).await;
    combat::forget_ship(ship);
    combat::forget_ship(source);
    combat::forget_ship(unpiloted);
    db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&pilot]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&pilot]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
