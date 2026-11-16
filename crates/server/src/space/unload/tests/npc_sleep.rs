//! Real-world NPC sleep: strict stop, cold restore before publication, and the
//! shared cancellation/load ordering. A deterministic belt-marked cohort on a
//! real SDE system exercises the exact safe predicate; other-blocker systems
//! are skipped, never loosened.
use super::*;
use crate::space::system::npc_sleep::NpcSpawnOrigin;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::{Fit, Key};
use infinity_core::consts::{CATEGORY_ENTITY, GROUP_ASTEROID_BELT};

fn belt_candidates(sde: &Arc<crate::sde::Sde>) -> Vec<(i64, i64)> {
    sde.map.public_systems().filter_map(|system| {
        sde.map.celestials(system.solar_system_id)
            .find(|row| row.group_id == Some(i64::from(GROUP_ASTEROID_BELT)))
            .map(|belt| (system.solar_system_id, belt.item_id))
    }).collect()
}
fn npc_ball(id: i64, type_id: i32, pos: V3) -> Ball {
    Ball { id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        mode: MODE_STOP, radius: 50.0, signature_radius: None, mass: 1e6,
        max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos, vel: [0.0; 3], old_pos: pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        interactive: false, massive: true, warp: crate::space::WarpState::default(),
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}
/// Install, drain and score one marked quiescent belt cohort.
async fn install_sleepable_cohort(world: &Arc<World>, system: i64, belt: i64, ids: &[i64]) -> Result<(u64, i32), &'static str> {
    let sde = world.sde().clone();
    let type_id = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)).unwrap().type_id;
    let ids = ids.to_vec();
    let group = world.with(system, move |s| {
        for (index, &id) in ids.iter().enumerate() {
            s.insert(npc_ball(id, type_id, [5.0e16, index as f64 * 1.0e6, 0.0]));
            let fit = Fit::for_space_type(sde.clone(), type_id, Vec::new());
            let stats = ShipStats::from_fit(&fit, Key::Ship);
            s.install_condition(id, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
            assert!(s.add_npc(id, NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
                fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
                scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
            }, Arc::new(|_, _| None))));
        }
        let group = s.spawn_group(ids, |_| {});
        assert!(s.mark_npc_group_sleepable(group, NpcSpawnOrigin::Belt(belt)));
        group
    }).await.unwrap();
    // Poll real actor turns until the strict predicate settles (tick.rs:73).
    let mut blocked = None;
    for _ in 0..30 {
        assert!(world.after(system, 100).await);
        blocked = world.with(system, |s| s.unload_blocked()).await.expect("system actor stopped");
        if blocked != Some("actions") { break; }
    }
    blocked.map_or(Ok((group, type_id)), Err)
}

/// First belted public system; fail fast with the actual reason.
async fn sleepable_system(world: &Arc<World>, sde: &Arc<crate::sde::Sde>, ids: &[i64]) -> (i64, i64, u64, i32) {
    let (system, belt) = belt_candidates(sde).into_iter().min_by_key(|&(system, _)| system).expect("no belted public system");
    world.system(system).await.unwrap();
    let (group, type_id) = install_sleepable_cohort(world, system, belt, ids).await
        .unwrap_or_else(|reason| panic!("system {system} still blocks unload: {reason}"));
    (system, belt, group, type_id)
}
fn event_name(value: &Value) -> Option<Vec<u8>> {
    let Value::Tuple(outer) = value else { return None };
    let Some(Value::Tuple(inner)) = outer.get(1) else { return None };
    inner.first().and_then(|v| match v { Value::Bytes(name) => Some(name.clone()), _ => None })
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn npc_cohort_unloads_sleeps_and_restores_before_first_observer() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let world = World::new(db, sde.clone(), 0);
    let (id, belt, group, type_id) = sleepable_system(&world, &sde, &[-10, -11]).await;
    let old = world.running(id).unwrap();
    let (identity, gate) = (old.tx.downgrade(), Arc::downgrade(&old.gate));
    drop(old);
    assert!(matches!(world.unload_one(id, &gate, None).await.unwrap(), Outcome::Unloaded));
    assert!(identity.upgrade().is_none() && world.running(id).is_none());
    assert!(crate::poison::lock_or_exit(world.sleeping_npcs.lock()).contains_key(&id));
    // Cold load restores the cohort before the first initial SetState.
    let (tx, mut delivered) = mpsc::unbounded_channel();
    world.observe(id, 55, belt, tx).await.unwrap();
    let mut names = Vec::new();
    while let Ok(message) = delivered.try_recv() {
        if let SceneDelivery::Destiny(events) = message {
            names.extend(events.iter().filter_map(event_name));
        }
    }
    for forbidden in [&b"RemoveBalls"[..], &b"TerminalPlayDestructionEffect"[..], &b"OnTarget"[..]] {
        assert!(!names.iter().any(|name| name.as_slice() == forbidden), "sleep produced {forbidden:?}");
    }
    let state = world.with(id, move |s| (
        s.ball(-10).map(|b| (b.type_id, b.pos, b.health)),
        s.is_npc(-10) && s.is_npc(-11), s.group_remaining(group),
    )).await.unwrap();
    assert_eq!(state, (Some((type_id, [5.0e16, 0.0, 0.0], [1.0; 3])), true, 2));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn sleep_payload_survives_cancelled_coordinator_and_double_load() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let world = World::new(db, sde.clone(), 0);
    let (id, _belt, _group, _type_id) = sleepable_system(&world, &sde, &[-20]).await;
    let old = world.running(id).unwrap();
    let (identity, gate) = (old.tx.downgrade(), Arc::downgrade(&old.gate));
    drop(old);
    // A coordinator cancelled after admission never owns the payload.
    let moving = world.clone();
    let moving_gate = gate.clone();
    let coordinator = tokio::spawn(async move { moving.unload_one(id, &moving_gate, None).await });
    until(|| identity.upgrade().is_none()).await;
    coordinator.abort();
    until(|| crate::poison::lock_or_exit(world.sleeping_npcs.lock()).contains_key(&id)).await;
    assert!(world.running(id).is_none());
    let (first, second) = tokio::join!(world.system(id), world.system(id));
    first.unwrap();
    second.unwrap();
    assert_eq!(world.with(id, |s| s.balls().filter(|b| s.is_npc(b.id)).count()).await.unwrap(), 1);
    assert!(crate::poison::lock_or_exit(world.sleeping_npcs.lock()).is_empty());
}
