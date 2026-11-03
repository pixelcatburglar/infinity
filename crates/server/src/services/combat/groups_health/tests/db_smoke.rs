use super::*;
use super::fixture;
use std::sync::Arc;
use crate::{consts::{CATEGORY_ENTITY, CATEGORY_SHIP}, space::{Ball, World, WarpState, MODE_STOP}};

fn ball(id: i64, type_id: i32, pilot: Option<i64>) -> Ball {
    Ball {
        id, type_id, owner_id: pilot.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn check_notification(event: (String, Vec<Value>), pilot: i64, ship: i64, increase: bool) -> (f64, f64) {
    assert_eq!(event.0, "OnModuleAttributeChanges");
    let [Value::List(changes)] = event.1.as_slice() else { panic!("expected one attribute change list") };
    let [Value::Tuple(change)] = changes.as_slice() else { panic!("expected one attribute change") };
    assert_eq!(change.len(), 8);
    assert_eq!(&change[..4], &[Value::str("OnModuleAttributeChange"), Value::Int(pilot), Value::Int(ship), Value::Int(ATTR_CHARGE as i64)]);
    let (Value::Float(new), Value::Float(old)) = (&change[5], &change[6]) else { panic!("expected charge values") };
    assert!(if increase { new > old } else { new < old });
    (*old, *new)
}

#[test]
fn npc_take_replace_and_destroy_forget_combat_state_but_player_take_preserves_it() {
    use crate::space::{NpcBrain, NpcConfig, System};
    crate::tests::init_config();
    let mut system = System::default();
    let ids = [i64::MIN + 100, i64::MIN + 101, i64::MIN + 102, i64::MIN + 103];
    for &id in &ids {
        system.insert(ball(id, 1, None));
        assert!(system.add_npc(id, NpcBrain::new(None, NpcConfig {
            fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0,
            scan_resolution: 1.0, repairs: (0.0, 0.0, 1.0),
        }, Arc::new(|_| Box::pin(async {})))));
    }
    let player = i64::MIN + 104;
    system.insert(ball(player, 1, Some(1)));
    {
        let mut st = state();
        for id in ids.into_iter().chain([player]) {
            st.capacitor.insert(id, (0.5, 0));
            st.hp.insert(id, Hp { now: [0.5; 3], max: [1.0; 3], resonance: [[1.0; 4]; 3] });
            st.condition.insert(id, Condition {
                stats: ShipStats { max: [1.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 1.0 },
                now: [0.5; 3], shield_at: 0,
            });
        }
    }
    system.take(ids[0]).unwrap();
    system.insert(ball(ids[1], 1, None));
    system.destroy_ball(ids[2]).unwrap();
    system.remove_ball(ids[3]).unwrap();
    system.take(player).unwrap();
    let mut st = state();
    for id in ids {
        assert!(!st.capacitor.contains_key(&id));
        assert!(!st.condition.contains_key(&id));
        assert!(!st.hp.contains_key(&id));
    }
    assert!(st.capacitor.remove(&player).is_some());
    assert!(st.condition.remove(&player).is_some());
    assert!(st.hp.remove(&player).is_some());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_damage_rechecks_replacements_and_applies_live_damage() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let npc_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)).unwrap().type_id;
    let mut system = crate::space::System::default();
    let target = i64::MIN + 810;
    for pilot in [Some(42), None] {
        system.insert(ball(target, npc_type, None));
        assert!(system.ball(target).unwrap().char_id.is_none());
        system.take(target);
        let replacement = ball(target, ship_type, pilot);
        if pilot.is_some() { system.insert(replacement); } else { system.add_persistent_ship(replacement); }
        let hp = Hp { now: [10.0; 3], max: [10.0; 3], resonance: [[1.0; 4]; 3] };
        state().hp.insert(target, hp);
        assert!(apply_npc_damage(&mut system, &sde, target, [100.0, 0.0, 0.0, 0.0], Some(9)).is_none());
        assert_eq!(state().hp.remove(&target).unwrap().now, [10.0; 3]);
        let current = system.ball(target).unwrap();
        assert_eq!(current.char_id, pilot);
        assert_eq!(current.health, [1.0; 3]);
    }
    system.insert(ball(target, npc_type, None));
    state().hp.insert(target, Hp { now: [10.0; 3], max: [10.0; 3], resonance: [[1.0; 4]; 3] });
    assert!(matches!(apply_npc_damage(&mut system, &sde, target, [5.0, 0.0, 0.0, 0.0], None),
        Some(NpcDamage::Applied { dealt: 5.0, destroyed: None, .. })));
    assert_eq!(system.ball(target).unwrap().health, [0.5, 1.0, 1.0]);
    assert!(matches!(apply_npc_damage(&mut system, &sde, target, [30.0, 0.0, 0.0, 0.0], None),
        Some(NpcDamage::Applied { destroyed: Some(_), .. })));
    assert!(!system.contains(target));
    assert!(!state().hp.contains_key(&target));

    let spec = crate::space::drone::DroneSpec { type_id: npc_type, bandwidth: 1.0, scan_resolution: 1.0,
        control_range: 1.0, max_target_range: 1.0, optimal_range: 1.0, orbit_range: 1.0,
        duration_ms: 1.0, effect_id: None };
    system.restore_drone(ball(target, npc_type, None), spec, Arc::new(|_| Box::pin(async {})));
    assert!(matches!(apply_npc_damage(&mut system, &sde, target, [100.0, 0.0, 0.0, 0.0], None),
        Some(NpcDamage::Drone { .. })));
    assert!(system.contains(target));
    assert!(!state().hp.contains_key(&target));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn capacitor_db_smoke() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let select = |category| sde.types.iter().find(|t| t.category_id == Some(category)
        && t.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)
        && t.attribute(ATTR_RECHARGE_RATE).is_some_and(|v| v.is_finite() && v > 0.0))
        .expect("SDE must contain a type with capacitor capacity and recharge").type_id;
    let ship_type = select(CATEGORY_SHIP);
    let npc_type = select(CATEGORY_ENTITY);
    let npc_capacity = sde.types.attribute(npc_type, ATTR_CAPACITOR_CAPACITY).unwrap();
    let system = 1;
    let before = fixture::fingerprint(&db).await.unwrap();
    let fixture = fixture::create(&db, ship_type, system).await.unwrap();
    let npc = -fixture.ship;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(fixture.character, tx.clone());
        test_world.add_ship(system, ball(fixture.ship, ship_type, Some(fixture.character))).await.unwrap();
        test_world.add_ship(system, ball(npc, npc_type, None)).await.unwrap();
        let fit = Fit::load(&test_db, sde, fixture.character, fixture.ship).await.ok().expect("load fit");
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let tau = fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        assert!(tau.is_finite() && tau > 0.0);
        let tolerance = (capacity * 1e-12).max(1e-12);
        assert!(capacity.is_finite() && capacity > 0.0);
        let player = capacitor_snapshot(&test_db, &test_world, system, fixture.ship).await.ok().expect("snapshot player").unwrap();
        assert_eq!(player.capacity, capacity);
        assert_eq!(player.level, capacity);
        let target = capacitor_snapshot(&test_db, &test_world, system, npc).await.ok().expect("snapshot NPC").unwrap();
        assert_eq!((target.capacity, target.level), (npc_capacity, npc_capacity));
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: fixture.character, ship_id: fixture.ship, system_id: system };
        assert!(drain_capacitor(&cx, &fit, capacity / 2.0));
        check_notification(rx.try_recv().unwrap(), fixture.character, fixture.ship, false);
        assert!(!drain_capacitor(&cx, &fit, capacity * 2.0));
        assert!(rx.try_recv().is_err());
        let before_drain = filetime_now();
        let drained = drain_exact(&test_db, &test_world, system, fixture.ship, capacity * 2.0).await.ok().expect("drain player");
        assert!(drained > 0.0 && drained <= capacity);
        check_notification(rx.try_recv().unwrap(), fixture.character, fixture.ship, false);
        let empty = capacitor_snapshot(&test_db, &test_world, system, fixture.ship).await.ok().expect("snapshot drained player").unwrap();
        let after_snapshot = filetime_now();
        let elapsed = after_snapshot.saturating_sub(before_drain).max(0) as f64 / FILETIME_SECOND as f64;
        let recharge_bound = recharge(0.0, capacity, tau, elapsed);
        assert!(empty.level >= 0.0 && empty.level <= recharge_bound + tolerance);
        let accepted = inject(&test_db, &test_world, system, fixture.ship, capacity * 2.0).await.ok().expect("inject player");
        let (old, new) = check_notification(rx.try_recv().unwrap(), fixture.character, fixture.ship, true);
        assert!(accepted > 0.0 && accepted <= capacity);
        assert!((accepted - (new - old)).abs() <= tolerance);
        assert!((new - capacity).abs() <= tolerance);
        let full = capacitor_snapshot(&test_db, &test_world, system, fixture.ship).await.ok().expect("snapshot injected player").unwrap();
        assert_eq!(full.level, capacity);
        assert_eq!(inject(&test_db, &test_world, system, fixture.ship, capacity).await.ok().expect("inject full player"), 0.0);
        assert!(rx.try_recv().is_err());
        assert_eq!(drain_exact(&test_db, &test_world, system, npc, npc_capacity * 2.0).await.ok().expect("drain NPC"), npc_capacity);
        let npc_accepted = inject(&test_db, &test_world, system, npc, npc_capacity * 2.0).await.ok().expect("inject NPC");
        assert!(npc_accepted > 0.0 && npc_accepted <= npc_capacity);
        let npc_full = capacitor_snapshot(&test_db, &test_world, system, npc).await.ok().expect("snapshot injected NPC").unwrap();
        assert_eq!(npc_full.level, npc_capacity);
        assert!(rx.try_recv().is_err());
    }).await;
    world.remove_ball(system, fixture.ship).await;
    world.remove_ball(system, npc).await;
    world.unregister_pilot(fixture.character);
    let flush = world.flush_positions().await;
    { let mut state = state(); state.capacitor.remove(&fixture.ship); state.capacitor.remove(&npc); }
    drop(world);
    fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_deaths_record_distinct_occurrences_and_exclude_other_balls() {
    use crate::dao::map_telemetry::{self, EventKind, EventSource, HistoryStat};
    use crate::space::{NpcBrain, NpcConfig};
    use crate::services::drones::tests::fixture::Fixture;

    let f = Fixture::new().await;
    let system = f.system;
    let owner = f.owner;
    let npc = -f.ship;
    let other = npc - 1;
    let npc_type = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)
        && t.attribute(ATTR_ENTITY_KILL_BOUNTY).is_some_and(|v| v > 0.0)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.group_id.and_then(|group| f.sde.wrecks.get(group, t.race_id.unwrap_or(0))).is_some())
        .expect("bounty-bearing NPC with wreck").type_id;
    let drone_type = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(crate::consts::CATEGORY_DRONE)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0))
        .expect("published drone with structure").type_id;
    let drone = f.drone(drone_type, 1, true).await;
    let marker = map_telemetry::next_occurrence(&f.db).await.unwrap();
    let start = filetime_now();
    let world2 = World::new(f.db.clone(), f.sde.clone(), 1);
    let (db, world, second) = (f.db.clone(), f.world.clone(), world2.clone());
    let result = tokio::spawn(async move {
        let add_npc = |world: Arc<World>, id| async move {
            let mut b = ball(id, npc_type, None);
            b.owner_id = owner;
            world.add_ship(system, b).await.unwrap();
            assert!(world.with(system, move |s| s.add_npc(id, NpcBrain::new(None, NpcConfig {
                fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0,
                scan_resolution: 1.0, repairs: (0.0, 0.0, 1.0),
            }, Arc::new(|_| Box::pin(async {}))))).await.unwrap());
        };
        let events = || async {
            db.query("SELECT source_id, solar_system_id, kind FROM game.map_events WHERE source=$1 AND source_id>$2 AND solar_system_id=$3 ORDER BY source_id",
                &[&(EventSource::NpcOccurrence as i16), &marker, &system]).await.unwrap()
        };
        add_npc(world.clone(), npc).await;
        damage(&db, &world, system, npc, [1e30; 4], None, None).await.ok().unwrap();
        assert!(world.ball(system, npc).await.is_none());
        assert_eq!(events().await.len(), 1);
        assert_eq!(damage(&db, &world, system, npc, [1e30, 0.0, 0.0, 0.0], None, None).await.ok().unwrap(), 0.0);
        assert_eq!(events().await.len(), 1);

        let balance = crate::dao::wallets::characters::balance(&db, owner).await.unwrap();
        add_npc(second.clone(), npc).await;
        damage(&db, &second, system, npc, [1e30; 4], Some(owner), None).await.ok().unwrap();
        assert!(second.ball(system, npc).await.is_none());
        assert!(crate::dao::wallets::characters::balance(&db, owner).await.unwrap() > balance);
        assert!(db.query_one("SELECT count(*) FROM game.items WHERE owner_id=$1 AND location_id=$2 AND item_id<>$3",
            &[&owner, &system, &drone]).await.unwrap().get::<_, i64>("count") >= 2);
        let rows = events().await;
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].get::<_, i64>("source_id"), rows[1].get::<_, i64>("source_id"));
        assert!(rows.iter().all(|row| row.get::<_, i64>("solar_system_id") == system
            && row.get::<_, i16>("kind") == EventKind::Npc as i16));
        let history = map_telemetry::history(&db, HistoryStat::Kills, start, filetime_now()).await.unwrap();
        let row = history.iter().find(|row| row.solar_system_id == system).unwrap();
        assert_eq!((row.value1, row.value2, row.value3), (2, 2, 0));

        let mut b = ball(other, npc_type, None);
        b.owner_id = owner;
        world.add_ship(system, b).await.unwrap();
        assert!(!world.with(system, move |s| s.is_npc(other)).await.unwrap());
        damage(&db, &world, system, other, [1e30; 4], None, None).await.ok().unwrap();
        assert!(world.ball(system, other).await.is_none());
        assert_eq!(events().await.len(), 2);

        db.execute("UPDATE game.items SET location_id=$2, flag_id=0, x=0, y=0, z=0 WHERE item_id=$1 AND owner_id=$3", &[&drone, &system, &owner]).await.unwrap();
        let spec = crate::space::drone::DroneSpec { type_id: drone_type, bandwidth: 1.0, scan_resolution: 1.0,
            control_range: 1.0, max_target_range: 1.0, optimal_range: 1.0, orbit_range: 1.0,
            duration_ms: 1.0, effect_id: None };
        let mut b = ball(drone, drone_type, None);
        b.owner_id = owner;
        world.with(system, move |s| s.restore_drone(b, spec, Arc::new(|_| Box::pin(async {})))).await.unwrap();
        damage(&db, &world, system, drone, [1e30, 1e30, 1e30, 1e30], None, None).await.ok().unwrap();
        assert!(world.ball(system, drone).await.is_none());
        assert!(db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&drone]).await.unwrap().is_none());
        assert_eq!(events().await.len(), 2);
    }).await;
    if let Some(ids) = world2.with(system, |s| s.balls().map(|b| b.id).collect::<Vec<_>>()).await {
        for id in ids { world2.remove_ball(system, id).await; forget_ship(id); }
    }
    world2.flush_positions().await.unwrap();
    f.db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id>$2 AND solar_system_id=$3",
        &[&(EventSource::NpcOccurrence as i16), &marker, &system]).await.unwrap();
    f.cleanup().await;
    result.unwrap();
}
