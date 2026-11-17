//! NPC sleep marking at real content entry points: belt dedupe and cooldown,
//! plus combat-site cohorts; unmarked and mining-clearance groups stay strict
//! unload blockers.
use super::*;
use crate::space::{SceneDelivery, HOSTILE_TO_ALL};
use crate::space::system::npc_sleep::NpcSpawnOrigin;
use crate::services::combat::damage::{apply_damage_guarded, DamageSource};
use crate::lua::events::resources::FactOrigin;
use infinity_core::consts::{CATEGORY_ENTITY, dogma::ATTR_ENTITY_ARMOR_REPAIR_AMOUNT, dogma::ATTR_ENTITY_SHIELD_BOOST_AMOUNT, dogma::EFFECT_NPC_BEHAVIOR_ARMOR_REPAIR, dogma::EFFECT_NPC_BEHAVIOR_SHIELD_BOOST};
use marshal::Value;

fn real_belt(sde: &Arc<crate::sde::Sde>) -> (i64, i64, V3) {
    for system in sde.map.public_systems() {
        if let Some(row) = sde.map.celestials(system.solar_system_id)
            .find(|row| row.group_id == Some(i64::from(GROUP_ASTEROID_BELT))) {
            return (system.solar_system_id, row.item_id,
                [row.x.unwrap_or(0.0), row.y.unwrap_or(0.0), row.z.unwrap_or(0.0)]);
        }
    }
    panic!("no belted public system in SDE");
}

/// Sorted live NPC balls after every queued spawn/mark post has run.
async fn npc_ids(world: &Arc<World>, system: i64) -> Vec<i64> {
    let mut ids = world.with(system, |s| {
        s.balls().filter(|b| s.is_npc(b.id)).map(|b| b.id).collect::<Vec<_>>()
    }).await.unwrap_or_default();
    ids.sort_unstable();
    ids
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn belt_landing_marks_group_and_dedupes_roster() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (system, belt, center) = real_belt(&sde);
    let world = Arc::new(World::new(db.clone(), sde, 0));
    belt_rats(&db, &world, 1, system, belt, center).await.unwrap();
    let roster = npc_ids(&world, system).await;
    assert!(!roster.is_empty() && state().belts.contains(&belt));
    belt_rats(&db, &world, 2, system, belt, center).await.unwrap();
    assert_eq!(npc_ids(&world, system).await, roster); // dedupe: no second roster
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn cleared_belt_cooldown_holds_dedupe_until_scheduled_release() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let (system, belt, center) = real_belt(&sde);
    let world = Arc::new(World::new(db.clone(), sde, 0));
    belt_rats(&db, &world, 1, system, belt, center).await.unwrap();
    let roster = npc_ids(&world, system).await;
    assert!(!roster.is_empty());
    for id in &roster { world.remove_ball(system, *id).await.unwrap(); }
    // The real last removal started the original cooldown: the marker is held
    // rather than cleared, so a landing during cooldown still dedupes. Expiry
    // itself rides the unchanged scheduled path and needs real wall time.
    assert!(state().belts.contains(&belt));
    belt_rats(&db, &world, 2, system, belt, center).await.unwrap();
    assert_eq!(npc_ids(&world, system).await, Vec::<i64>::new());
}

// ---------------------------------------------------------------- site cohorts

/// A live combat site with `npcs` deterministic no-repair, wreck-free NPCs
/// admitted through the same `combat::npc::spawn` `spawn_site_npcs` uses.
/// Group registration stays with each scenario to pass its own `origin`.
async fn site_cohort(npcs: usize, save: bool) -> (Db, Arc<World>, i64, Site, Vec<i64>) {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let system = sde.map.public_systems().next().expect("public system").solar_system_id;
    let type_id = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)
        && t.attribute(ATTR_ENTITY_SHIELD_BOOST_AMOUNT).unwrap_or(0.0) <= 0.0
        && t.attribute(ATTR_ENTITY_ARMOR_REPAIR_AMOUNT).unwrap_or(0.0) <= 0.0
        && t.effects().iter().all(|entry| entry.effect_id != EFFECT_NPC_BEHAVIOR_SHIELD_BOOST
            && entry.effect_id != EFFECT_NPC_BEHAVIOR_ARMOR_REPAIR)
        && t.group_id.and_then(|g| sde.wrecks.get(g, t.race_id.unwrap_or(0))).is_none())
        .expect("SDE entity type without local repair or wreck").type_id;
    let world = World::new(db.clone(), sde, 0);
    world.system(system).await.unwrap();
    let mut site = new_site(system, [0.0; 3], Kind::Anomaly, STRENGTH_ALL, 1.0, &mut HashSet::new());
    site.dungeon_id = Some(world.sde().dungeons.iter().next().expect("SDE dungeon").id);
    if save {
        site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
        save_site(&db, &site).await.unwrap();
    }
    let mut ids = Vec::new();
    for i in 0..npcs {
        let data = world.sde().types.get(type_id).expect("picked type");
        ids.push(crate::services::combat::npc::spawn(&db, &world, system, data, 1000127,
            [8.0e15, 1.0e6 * (i + 1) as f64, 0.0], now_ms(), Some(HOSTILE_TO_ALL)).await.unwrap());
    }
    site.npcs = Some(ids.clone());
    state().sites.insert(site.id, site.clone());
    (db, world, system, site, ids)
}

/// Bounded wait for the real strict stop: retries the unchanged sweep while
/// real flush and settle work drains (~50 s bound), then panics with the
/// last exact `unload_blocked` reason.
async fn unloaded(world: &World, system: i64) -> bool {
    let mut reason = None;
    for _ in 0..200 {
        if world.unload_idle_systems().await.unwrap().unloaded > 0
            && world.with(system, |_| ()).await.is_none() { return true; }
        reason = world.with(system, |s| s.unload_blocked()).await.flatten().or(reason);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("strict stop never admitted the cohort: {reason:?}")
}

fn set_state_bag(rx: &mut tokio::sync::mpsc::UnboundedReceiver<SceneDelivery>) -> Value {
    while let Ok(SceneDelivery::Destiny(events)) = rx.try_recv() {
        for event in events {
            let Value::Tuple(outer) = event else { continue };
            let Some(Value::Tuple(inner)) = outer.into_iter().nth(1) else { continue };
            if inner.first() != Some(&Value::str("SetState")) { continue; }
            if let Some(Value::Tuple(args)) = inner.into_iter().nth(1) {
                if let Some(bag) = args.into_iter().next() { return bag; }
            }
        }
    }
    panic!("initial SetState not delivered");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and INFINITY_TEST_ENABLE_IDLE_STOP in this exact process; run serially"]
async fn site_survivors_restore_before_initial_state_without_respawn() {
    assert!(crate::tests::init_config().gm.performance_testing, "requires INFINITY_TEST_ENABLE_IDLE_STOP");
    let (db, world, system, site, ids) = site_cohort(2, true).await;
    let (dead, survivor) = (ids[0], ids[1]);
    let row = |s: crate::dao::exploration::Site| (s.id, s.in_space, s.containers, s.hacked);
    let before = crate::dao::exploration::sites(&db, system).await.unwrap().into_iter().map(row).find(|(id, _, _, _)| *id == site.id);
    let loaded = state().loaded.contains(&system);
    track_site_clearance(db.clone(), world.clone(), site.id, system, ids.clone(), Some(NpcSpawnOrigin::Exploration(site.id)));
    // A real non-lethal hit, then a real lethal settle; the dead id stays in `Site.npcs`.
    let source = world.with(system, move |s| s.prepare_body_combat_source(dead, None, None, None, FactOrigin::Natural, |_| true))
        .await.unwrap().unwrap_or_else(|_| panic!("NPC source refused"));
    let hit = apply_damage_guarded(&db, &world, system, survivor, [1.0, 0.0, 0.0, 0.0],
        DamageSource { facts: Some(source), ..Default::default() }, false, |_| true).await.unwrap().expect("real hit admitted");
    assert!(hit.dealt > 0.0 && !hit.lethal, "fixture hit must deal damage without killing");
    let since = crate::client::filetime_now();
    assert!(apply_damage_guarded(&db, &world, system, dead, [0.0; 4],
        DamageSource { force_origin: Some(FactOrigin::Gm), ..Default::default() }, true, |_| true).await.unwrap().expect("lethal admitted").lethal);
    let (dead_gone, before_health) = world.with(system, move |s|
        (s.ball(dead).is_none() && !s.is_npc(dead), s.ball(survivor).map(|b| b.health))).await.unwrap();
    assert!(dead_gone && before_health.is_some_and(|h| h.iter().any(|v| *v < 1.0))
        && state().sites[&site.id].npcs == Some(ids.clone()), "dead stays out, damaged survivor stays, roster unchanged");
    assert!(unloaded(&world, system).await, "strict stop never admitted the cohort");
    let (tx, mut delivered) = tokio::sync::mpsc::unbounded_channel();
    world.observe(system, 7, survivor, tx).await.unwrap(); // cold load + first SetState
    let bag = set_state_bag(&mut delivered);
    let Some(Value::List(slims)) = crate::services::value_field(&bag, "slims").cloned() else { panic!("slims") };
    let slim_ids: Vec<i64> = slims.iter().filter_map(|slim| match crate::services::value_field(slim, "itemID") {
        Some(Value::Int(id)) => Some(*id), _ => None }).collect();
    assert!(slim_ids.contains(&survivor) && !slim_ids.contains(&dead),
        "survivor only in the first scene: slim_ids={slim_ids:?} survivor={survivor} dead={dead} survivor_ball={:?}",
        world.with(system, move |s| s.ball(survivor).is_some()).await);
    let Some(Value::Tuple(layers)) = crate::services::value_field(&bag, "damageState").and_then(|d| match d {
        Value::Dict(rows) => rows.iter().find(|(k, _)| *k == Value::Int(survivor)).map(|(_, v)| v.clone()), _ => None }) else { panic!("survivor damageState") };
    let (Value::Tuple(shield), Value::Float(armor), Value::Float(hull)) = (&layers[0], layers[1].clone(), layers[2].clone()) else { panic!("damage layers") };
    let Some(&Value::Float(shield_now)) = shield.first() else { panic!("shield fraction") };
    assert_eq!(Some((shield_now, armor, hull)), before_health.map(|h| (h[0], h[1], h[2])),
        "restored damageState must equal the pre-unload health exactly");
    landing::arrive(&db, &world, 9, system, site.id).await.unwrap();
    assert_eq!(npc_ids(&world, system).await, vec![survivor], "no respawn, no resurrection");
    let after = crate::dao::exploration::sites(&db, system).await.unwrap().into_iter().map(row).find(|(id, _, _, _)| *id == site.id);
    assert_eq!(after, before, "site row untouched by sleep and wake");
    assert!(state().sites[&site.id].npcs == Some(ids.clone()) && state().loaded.contains(&system) == loaded);
    state().sites.remove(&site.id);
    delete_site(&db, site.id).await.unwrap();
    db.execute("DELETE FROM game.map_events WHERE solar_system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE occurred_at >= $1)", &[&since]).await.unwrap();
    assert_eq!(db.execute("DELETE FROM game.progress_events WHERE occurred_at >= $1", &[&since]).await.unwrap(), 1, "one death journal, none from sleep/wake");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and INFINITY_TEST_ENABLE_IDLE_STOP in this exact process; run serially"]
async fn sleeping_site_callback_does_not_keep_world_alive() {
    assert!(crate::tests::init_config().gm.performance_testing, "requires INFINITY_TEST_ENABLE_IDLE_STOP");
    let (db, world, system, site, ids) = site_cohort(1, false).await;
    track_site_clearance(db, world.clone(), site.id, system, ids, Some(NpcSpawnOrigin::Exploration(site.id)));
    assert!(unloaded(&world, system).await, "cohort never reached the sleep store");
    let weak = Arc::downgrade(&world);
    state().sites.remove(&site.id);
    drop(world);
    assert!(weak.upgrade().is_none(), "sleep payload must not hold a strong World");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and INFINITY_TEST_ENABLE_IDLE_STOP in this exact process; run serially"]
async fn site_clearance_waits_for_last_survivor_and_original_linger() {
    assert!(crate::tests::init_config().gm.performance_testing, "requires INFINITY_TEST_ENABLE_IDLE_STOP");
    let (db, world, system, site, ids) = site_cohort(1, true).await;
    let baseline: HashSet<i64> = crate::dao::exploration::sites(&db, system).await.unwrap().into_iter().map(|s| s.id).collect();
    track_site_clearance(db.clone(), world.clone(), site.id, system, ids.clone(), Some(NpcSpawnOrigin::Exploration(site.id)));
    assert!(unloaded(&world, system).await, "strict stop never admitted the cohort");
    assert!(crate::dao::exploration::sites(&db, system).await.unwrap().iter().any(|s| s.id == site.id), "sleep runs no clear");
    world.system(system).await.unwrap();
    assert_eq!(npc_ids(&world, system).await, ids);
    // The real last removal clears the group and queues the original linger; an
    // already-empty registration also clears once at spawn and never marks.
    world.remove_ball(system, ids[0]).await.unwrap();
    let mut empty = new_site(system, [1.0, 0.0, 0.0], Kind::Anomaly, STRENGTH_ALL, 1.0, &mut HashSet::new());
    empty.dungeon_id = site.dungeon_id;
    empty.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    let empty_id = empty.id;
    save_site(&db, &empty).await.unwrap();
    state().sites.insert(empty_id, empty);
    track_site_clearance(db.clone(), world.clone(), empty_id, system, vec![], Some(NpcSpawnOrigin::Exploration(empty_id)));
    assert!(state().sites.contains_key(&site.id) && state().sites.contains_key(&empty_id));
    assert!(crate::dao::exploration::sites(&db, system).await.unwrap().iter().any(|s| s.id == site.id || s.id == empty_id));
    landing::arrive(&db, &world, 9, system, site.id).await.unwrap();
    assert!(state().sites[&site.id].npcs.is_some(), "no respawn while lingering");
    assert_eq!(world.unload_idle_systems().await.map(|r| (r.unloaded, r.failed)).unwrap_or_default(), (0, 0), "pending linger stays busy");
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        while crate::dao::exploration::sites(&db, system).await.unwrap().iter().any(|s| s.id == site.id || s.id == empty_id) {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }).await.expect("original linger then the deferred replacement");
    assert!(!state().sites.contains_key(&site.id) && !state().sites.contains_key(&empty_id));
    for row in crate::dao::exploration::sites(&db, system).await.unwrap().into_iter().filter(|s| !baseline.contains(&s.id)) {
        state().sites.remove(&row.id);
        delete_site(&db, row.id).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and INFINITY_TEST_ENABLE_IDLE_STOP in this exact process; run serially"]
async fn mining_and_unmarked_mission_groups_remain_unload_blockers() {
    assert!(crate::tests::init_config().gm.performance_testing, "requires INFINITY_TEST_ENABLE_IDLE_STOP");
    let (db, world, system, site, ids) = site_cohort(2, false).await;
    // A mission-style group never carries an origin; both mining callers share
    // this exact helper call with `None` and stay unmarked.
    let mission = world.with(system, { let m = vec![ids[0]]; move |s| s.spawn_group(m, |_| {}) }).await.unwrap();
    track_site_clearance(db.clone(), world.clone(), site.id, system, vec![ids[1]], None);
    assert_eq!(world.unload_idle_systems().await.map(|r| (r.unloaded, r.failed)).unwrap_or_default(), (0, 0), "unmarked cohorts stay strict blockers");
    assert_eq!(world.with(system, move |s| (s.group_remaining(mission), s.balls().filter(|b| s.is_npc(b.id)).count())).await.unwrap(), (1, 2));
    assert!(!npc_sleep_origin_matches(system, site.id + 100), "absent site is never an origin");
    state().sites.remove(&site.id);
}
