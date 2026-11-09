use super::*;
use super::fixture;
use std::sync::Arc;
use infinity_core::{consts::{CATEGORY_ENTITY, CATEGORY_SHIP}};
use crate::{space::{Ball, World, WarpState, MODE_STOP}};

fn ball(id: i64, type_id: i32, pilot: Option<i64>) -> Ball {
    Ball {
        id, type_id, owner_id: pilot.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
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
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn actor_damage_keeps_checkpoint_unchanged_until_flush_and_records_positive_source() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|value| value > 0.0))
        .expect("SDE must contain a ship with structure").type_id;
    let system = 1;
    let f = fixture::create(&db, ship_type, system).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL WHERE character_id=$1",
        &[&f.character, &f.ship, &system]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    test_world.system(system).await.unwrap();
    let row = crate::dao::items::space_ships::FreeShip {
        item_id: f.ship, type_id: ship_type, owner_id: f.character,
        corporation_id: 1, alliance_id: -1, pos: [0.0; 3],
    };
    let prepared = crate::services::ship::balls::piloted(&test_db, sde, &row, f.character, 0.0).await.ok().expect("prepare pilot ship");
    test_world.with(system, move |s| prepared.install(s, |s, b| s.add_persistent_ship(b))).await.unwrap();
    let world = test_world;
    let db = test_db;
    let before = crate::dao::items::dogma::overrides(&db, f.ship).await.unwrap();
    let generation = crate::space::targeting::next_generation();
    let weak = Arc::downgrade(&world);
    let mut attempt = world.with(system, move |s| {
        let facts = s.prepare_combat_source(f.ship, None, None, Some(ship_type),
            crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1,
            move |s| s.pilot_ball(f.character).is_some_and(|ball| ball.id == f.ship)).unwrap();
        crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system)
    }).await.unwrap();
    let source = crate::services::combat::damage::DamageSource {
        attacker_id: Some(f.character), source_ship_id: Some(f.ship),
        source_ship_type: Some(ship_type), weapon_type_id: Some(ship_type),
        facts: Some(attempt.facts()), illegal: true, ..Default::default()
    };
    let outcome = crate::services::combat::damage::apply_damage_guarded(&db, &world, system, f.ship, [1.0, 0.0, 0.0, 0.0], source, false, |_| true).await.unwrap().unwrap();
    attempt.disarm();
    assert!(outcome.dealt > 0.0);
    let snapshot = world.with(system, move |s| s.combat_condition_snapshot(f.ship).unwrap()).await.unwrap();
    assert_eq!(snapshot.contributions.len(), 1);
    assert!(snapshot.contributions[0].illegal);
    let during = crate::dao::items::dogma::overrides(&db, f.ship).await.unwrap();
    let values = |rows: &[crate::dao::items::dogma::AttributeOverride]| rows.iter().map(|row| (row.attribute_id, row.value, row.updated_at)).collect::<Vec<_>>();
    assert_eq!(values(&during), values(&before));
    world.flush_positions().await.unwrap();
    let after = crate::dao::items::dogma::overrides(&db, f.ship).await.unwrap();
    assert_ne!(values(&after), values(&before));
    }).await;
    world.remove_ball(system, f.ship).await;
    world.flush_positions().await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
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
    world.unregister_pilot_generation(fixture.character, None);
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
    let npc = 9_000_000_000_000_i64 + f.ship;
    let ship = f.ship;
    let pilot = f.world.ball(system, ship).await.unwrap();
    let ship_type = f.sde.types.get(pilot.type_id).unwrap().type_id;
    let other = crate::dao::items::creation::positioned(&f.db, ship_type, owner, system, [0.0; 3]).await.unwrap();
    let npc_type = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ENTITY)
        && t.attribute(ATTR_ENTITY_KILL_BOUNTY).is_some_and(|v| v > 0.0)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0)
        && t.group_id.and_then(|group| f.sde.wrecks.get(group, t.race_id.unwrap_or(0))).is_some())
        .expect("bounty-bearing NPC with wreck").type_id;
    let drone_type = f.sde.types.iter().find(|t| t.published == Some(true)
        && t.category_id == Some(infinity_core::consts::CATEGORY_DRONE)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|v| v > 0.0))
        .expect("published drone with structure").type_id;
    let drone = f.drone(drone_type, 1, true).await;
    let marker = map_telemetry::next_occurrence(&f.db).await.unwrap();
    let start = filetime_now();
    let db = f.db.clone();
    let sde = f.sde.clone();
    let world2 = World::new(db.clone(), sde.clone(), 1);
    let (world, second) = (f.world.clone(), world2.clone());
    let self_kill_events = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let captured = self_kill_events.clone();
    let result = tokio::spawn(async move {
        let add_npc = |world: Arc<World>, id| async move {
            let mut b = ball(id, npc_type, None);
            b.owner_id = owner;
            world.add_ship(system, b).await.unwrap();
            assert!(world.with(system, move |s| s.add_npc(id, NpcBrain::new(None, NpcConfig {
                fly_range: 1.0, aggro_range: 1.0, rate_of_fire_ms: 1.0,
                scan_resolution: 1.0, repairs: (0.0, 0.0, 1.0),
            }, Arc::new(|_, _| Some(Box::pin(async {})))))).await.unwrap());
        };
        let events = || async {
            db.query("SELECT source_id, solar_system_id, kind FROM game.map_events WHERE source=$1 AND source_id>$2 AND solar_system_id=$3 ORDER BY source_id",
                &[&(EventSource::NpcOccurrence as i16), &marker, &system]).await.unwrap()
        };
        let player_source = |world: Arc<World>| async move {
            let weak = Arc::downgrade(&world);
            let generation = crate::space::targeting::next_generation();
            world.with(system, move |s| {
                let facts = s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural,
                    generation, 1, 1, move |s| s.pilot_ball(owner).is_some_and(|ball| ball.id == ship)).unwrap();
                crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system)
            }).await.unwrap()
        };
        add_npc(world.clone(), npc).await;
        let weak = Arc::downgrade(&world);
        let mut attempt = world.with(system, move |s| {
            let facts = s.prepare_body_combat_source(npc, None, None, None,
                crate::lua::events::resources::FactOrigin::Natural, move |s| s.is_npc(npc)).unwrap();
            crate::services::effects::space_source::PreparedCombatAttempt::new(facts, weak, system)
        }).await.unwrap();
        crate::services::combat::damage::apply_damage_guarded(&db, &world, system, npc, [1e30; 4], crate::services::combat::damage::DamageSource {
            facts: Some(attempt.facts()), ..Default::default()
        }, false, |_| true).await.unwrap().unwrap();
        attempt.disarm();
        let rows = db.query("SELECT event_id::text AS id, kind FROM game.progress_events WHERE subject_character_id IS NULL AND ((kind='DamageApplied' AND (payload->'DamageApplied'->'source'->>'item_id')::bigint=$1 AND (payload->'DamageApplied'->'target'->>'item_id')::bigint=$1 AND payload->'DamageApplied'->'source'->'incarnation'=payload->'DamageApplied'->'target'->'incarnation' AND (payload->'DamageApplied'->'context'->>'system_id')::bigint=$2) OR (kind='NpcDestroyed' AND (payload->'NpcDestroyed'->'npc'->>'item_id')::bigint=$1 AND (payload->'NpcDestroyed'->>'system_id')::bigint=$2))",
            &[&npc, &system]).await.unwrap();
        let owned: Vec<(String, String)> = rows.iter().map(|row| (row.get("id"), row.get("kind"))).collect();
        *captured.lock().unwrap() = owned.clone();
        assert_eq!(owned.iter().filter(|(_, kind)| kind == "NpcDestroyed").count(), 1);
        assert!(owned.iter().any(|(_, kind)| kind == "DamageApplied"));
        assert!(world.ball(system, npc).await.is_none());
        assert_eq!(events().await.len(), 1);
        assert!(crate::services::combat::damage::apply_damage_guarded(&db, &world, system, npc, [1e30, 0.0, 0.0, 0.0], Default::default(), false, |_| true).await.unwrap().is_none());
        assert_eq!(events().await.len(), 1);

        let balance = crate::dao::wallets::characters::balance(&db, owner).await.unwrap();
        second.add_ship(system, pilot).await.unwrap();
        let fit = Fit::load(&db, sde.clone(), owner, ship).await.ok().expect("load pilot fit");
        let condition = crate::services::combat::prepare_condition_with_fit(&db, ship, &fit).await.ok().expect("load pilot condition");
        second.with(system, move |s| s.install_condition(ship, condition.into_condition(), fit)).await.unwrap();
        add_npc(second.clone(), npc).await;
        let mut attempt = player_source(second.clone()).await;
        crate::services::combat::damage::apply_damage_guarded(&db, &second, system, npc, [1e30; 4], crate::services::combat::damage::DamageSource {
            attacker_id: Some(owner), attacker_ball_id: Some(ship), source_ship_id: Some(ship),
            source_ship_type: Some(ship_type), facts: Some(attempt.facts()), ..Default::default()
        }, false, |_| true).await.unwrap().unwrap();
        attempt.disarm();
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

        let row = crate::dao::items::space_ships::FreeShip {
            item_id: other, type_id: ship_type, owner_id: owner,
            corporation_id: 1, alliance_id: -1, pos: [0.0; 3],
        };
        let prepared = crate::services::ship::balls::unpiloted(&db, sde.clone(), &row).await.ok().expect("prepare unpiloted ship");
        world.with(system, move |s| prepared.install(s, |s, b| s.add_persistent_ship(b))).await.unwrap();
        assert!(world.with(system, move |s| s.persistent_generation(other).is_some()
            && s.combat_membership_generation(other).is_some()).await.unwrap());
        assert!(!world.with(system, move |s| s.is_npc(other)).await.unwrap());
        let mut attempt = player_source(world.clone()).await;
        crate::services::combat::damage::apply_damage_guarded(&db, &world, system, other, [1e30; 4], crate::services::combat::damage::DamageSource {
            attacker_id: Some(owner), attacker_ball_id: Some(ship), source_ship_id: Some(ship),
            source_ship_type: Some(ship_type), facts: Some(attempt.facts()), ..Default::default()
        }, false, |_| true).await.unwrap().unwrap();
        attempt.disarm();
        assert!(world.ball(system, other).await.is_none());
        assert_eq!(events().await.len(), 2);

        db.execute("UPDATE game.items SET location_id=$2, flag_id=0, x=0, y=0, z=0 WHERE item_id=$1 AND owner_id=$3", &[&drone, &system, &owner]).await.unwrap();
        let spec = crate::space::drone::DroneSpec { type_id: drone_type, bandwidth: 1.0, scan_resolution: 1.0,
            control_range: 1.0, max_target_range: 1.0, optimal_range: 1.0, orbit_range: 1.0,
            duration_ms: 1.0, effect_id: None };
        let mut b = ball(drone, drone_type, None);
        b.owner_id = owner;
        let fit = Fit::for_space_type(sde.clone(), drone_type, Vec::new());
        let condition = prepare_condition_with_fit_key(&db, drone, &fit, Key::Ship).await.unwrap();
        let prepared = crate::space::drone::PreparedDrone { ball: b, spec, condition, fit, key: Key::Ship };
        world.with(system, move |s| s.restore_drone(prepared, Arc::new(|_, _| Some(Box::pin(async {}))))).await.unwrap();
        let mut attempt = player_source(world.clone()).await;
        crate::services::combat::damage::apply_damage_guarded(&db, &world, system, drone, [1e30; 4], crate::services::combat::damage::DamageSource {
            attacker_id: Some(owner), attacker_ball_id: Some(ship), source_ship_id: Some(ship),
            source_ship_type: Some(ship_type), facts: Some(attempt.facts()), ..Default::default()
        }, false, |_| true).await.unwrap().unwrap();
        attempt.disarm();
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
    f.db.execute("DELETE FROM game.map_events WHERE source=$1 AND source_id=$2",
        &[&(EventSource::UnpilotedShip as i16), &other]).await.unwrap();
    let ids: Vec<String> = self_kill_events.lock().unwrap().iter().map(|(id, _)| id.clone()).collect();
    f.db.execute("DELETE FROM game.progress_deliveries WHERE event_id::text=ANY($1)", &[&ids]).await.unwrap();
    let deleted = f.db.execute("DELETE FROM game.progress_events WHERE event_id::text=ANY($1)", &[&ids]).await.unwrap();
    f.cleanup().await;
    assert_eq!(deleted as usize, ids.len(), "captured self-kill events were not all removed");
    result.unwrap();
}
