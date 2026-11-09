#[path = "tests/governance.rs"]
mod governance;
use super::*;
use crate::space::WarpState;

fn ball(id: i64, kind: i32, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: kind, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: pilot.is_some(), warp: WarpState::default(),
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn delivered_names(s: &mut System, rx: &mut mpsc::UnboundedReceiver<Vec<Value>>) -> Vec<Vec<u8>> {
    while let Some((stamp, action)) = s.actions.pop_front() { action(s, stamp); }
    let mut names = Vec::new();
    while let Ok(events) = rx.try_recv() {
        for event in events {
            let Value::Tuple(parts) = event else { panic!("event tuple") };
            let Value::Tuple(call) = &parts[1] else { panic!("event call") };
            let Value::Bytes(name) = &call[0] else { panic!("event name") };
            names.push(name.to_vec());
        }
    }
    names
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn authorization_checks_identity_category_occupancy_safety_and_surface_range() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let other = sde.types.iter().find(|t| t.category_id != Some(CATEGORY_SHIP)).unwrap().type_id;
    let mut s = System::new(1, Arc::default(), 1000);
    s.projection_sde = Some(sde);
    s.insert(ball(10, ship, Some(1), 0.0));
    s.add_persistent_ship(ball(20, ship, None, 12.0));
    let snapshot = s.authorize_board(1, 10, 20, 10.0).ok().unwrap();
    assert_eq!(snapshot.old.pos, [0.0; 3]);
    assert_eq!(snapshot.target.pos, [12.0, 0.0, 0.0]);
    assert!(matches!(s.authorize_board(1, 10, 20, 9.999), Err(BoardRefusal::OutOfRange)));
    assert!(matches!(s.authorize_board(1, 20, 10, 10.0), Err(BoardRefusal::StalePilot)));
    assert!(matches!(s.authorize_board(1, 10, 10, 10.0), Err(BoardRefusal::InvalidTarget)));
    s.balls.get_mut(&20).unwrap().owner_id = 2;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::InvalidTarget)));
    s.balls.get_mut(&20).unwrap().owner_id = 1;
    s.balls.get_mut(&20).unwrap().type_id = other;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::InvalidTarget)));
    s.balls.get_mut(&20).unwrap().type_id = ship;
    s.balls.get_mut(&20).unwrap().interactive = true;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Occupied)));
    s.balls.get_mut(&20).unwrap().interactive = false;
    s.balls.get_mut(&20).unwrap().char_id = Some(2);
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Occupied)));
    s.balls.get_mut(&20).unwrap().char_id = None;
    s.balls.get_mut(&20).unwrap().cloak = 1;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Unsafe)));
    s.balls.get_mut(&20).unwrap().cloak = 0;
    s.balls.get_mut(&10).unwrap().mode = MODE_WARP;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Unsafe)));
    s.balls.get_mut(&10).unwrap().mode = MODE_STOP;
    s.balls.get_mut(&20).unwrap().radius = f32::NAN;
    assert!(matches!(s.authorize_board(1, 10, 20, 10.0), Err(BoardRefusal::Unsafe)));
    s.balls.get_mut(&20).unwrap().radius = 1.0;
    s.take(20);
    s.add_persistent_ship(ball(20, ship, None, 0.0));
    assert_ne!(snapshot.target_generation, s.persistent_generation(20).unwrap());
    assert!(s.authorize_board(1, 10, 20, 0.0).is_ok());
    s.balls.get_mut(&10).unwrap().vel = [10.0, 0.0, 0.0];
    s.tick(2000);
    s.tick(2500);
    let sampled = s.authorize_board(1, 10, 20, 100.0).ok().unwrap();
    assert_eq!(sampled.old.pos, s.sample_position(10).unwrap());
    assert_ne!(sampled.old.pos, s.ball(10).unwrap().pos);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_publication_replaces_membership_and_preserves_observer() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && [infinity_core::consts::dogma::ATTR_ARMOR_HP, infinity_core::consts::dogma::ATTR_STRUCTURE_HP]
            .iter().all(|&attribute| t.attribute(attribute).is_some_and(|value| value > 0.0))).unwrap().type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let (account, owner, old_id, target_id) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 0) RETURNING character_id", &[&account, &station]).await?.get("character_id");
            let old = creation::positioned(tx, kind, owner, station, [0.0; 3]).await?;
            let target = creation::positioned(tx, kind, owner, station, [2.0, 0.0, 0.0]).await?;
            Ok::<_, crate::db::Error>((account, owner, old, target))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let row = |id, pos| FreeShip { item_id: id, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos };
    let old = balls::unpiloted(&db, sde.clone(), &row(old_id, [0.0; 3])).await.ok().unwrap();
    let target = balls::piloted(&db, sde.clone(), &row(target_id, [2.0, 0.0, 0.0]), owner, 0.0).await.ok().unwrap();
    let original = balls::piloted(&db, sde.clone(), &row(old_id, [0.0; 3]), owner, 0.0).await.ok().unwrap();
    let free_target = balls::unpiloted(&db, sde.clone(), &row(target_id, [2.0, 0.0, 0.0])).await.ok().unwrap();
    let mut s = System::new(1, Arc::default(), 1000);
    s.projection_sde = Some(sde.clone());
    original.install(&mut s, |s, ball| s.add_ship(ball));
    free_target.install(&mut s, |s, ball| s.add_persistent_ship(ball));
    let generation = crate::space::targeting::next_generation();
    let source = s.prepare_combat_source(old_id, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
    let damage = crate::services::combat::damage::DamageSource {
        attacker_id: source.actual_actor_character, attacker_ball_id: Some(source.entity.item_id),
        source_ship_id: source.context.ship_id, source_ship_type: source.context.ship_type_id,
        facts: Some(source), force_origin: Some(source.context.origin), ..Default::default()
    };
    let lethal = s.apply_damage(old_id, [0.0; 4], damage, true).unwrap().unwrap().first_lethal.unwrap();
    s.finish_combat_attempt(&source);
    s.retire_combat_source(generation).unwrap();
    assert!(s.begin_condition_transitions(&[old_id, target_id]).is_none());
    assert!(s.restore_known_rollback(old_id, lethal.token, lethal.generation));
    assert!(s.set_condition_fraction_mask(old_id, 0b110, 0.5));
    assert!(s.set_condition_fraction_mask(target_id, 0b110, 0.5));
    let old_condition = s.combat_condition_snapshot(old_id).unwrap().condition;
    let target_condition = s.combat_condition_snapshot(target_id).unwrap().condition;
    let (tx, mut rx) = mpsc::unbounded_channel();
    s.observers.insert(7, (old_id, tx));
    let snapshot = s.authorize_board(owner, old_id, target_id, 100.0).ok().unwrap();
    let old_generation = snapshot.old_generation;
    let target_generation = snapshot.target_generation;
    s.compressors.insert(old_id, HashMap::new());
    s.actions.clear();
    let queued = s.actions.len();
    let transition = s.begin_condition_transitions(&[old_id, target_id]).unwrap();
    let tokens = [transition.entries[0].0, transition.entries[1].0];
    let states = [transition.entries[0].1.clone(), transition.entries[1].1.clone()];
    let revisions = [states[0].revision, states[1].revision];
    assert!(matches!(s.apply_damage(old_id, [1.0; 4], Default::default(), false), Ok(None)));
    assert!(s.begin_condition_transitions(&[old_id]).is_none());
    assert!(s.apply_committed_transition(transitions::CommittedTransition {
        old: Some(transitions::CommittedOld::Retained { snapshot_ball: snapshot.old, prepared: old, state: states[0].clone(), token: tokens[0] }),
        target: transitions::CommittedTarget { id: target_id, sampled_position: snapshot.target.pos, prepared: target, pilot: Some((7, owner)), existing: Some((states[1].clone(), tokens[1])) },
        character: owner, expected: None, fresh: false, actor_gate: None,
    }));
    assert!(!s.compressors.contains_key(&old_id));
    assert_eq!(s.actions.len(), queued + 4);
    assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
    assert_eq!(s.ball(old_id).unwrap().char_id, None);
    assert!(!s.ball(old_id).unwrap().interactive);
    assert_eq!(s.observers[&7].0, target_id);
    assert_ne!(s.persistent_generation(old_id), Some(old_generation));
    assert_ne!(s.persistent_generation(target_id), Some(target_generation));
    assert!(!s.acknowledge_condition(old_id, old_generation, revisions[0]));
    assert!(!s.acknowledge_condition(target_id, target_generation, revisions[1]));
    assert_eq!(s.ball(old_id).unwrap().pos, [0.0; 3]);
    assert_eq!(s.ball(target_id).unwrap().pos, [2.0, 0.0, 0.0]);
    assert_eq!(s.ball(target_id).unwrap().vel, [0.0; 3]);
    for (id, before) in [(old_id, old_condition), (target_id, target_condition)] {
        let after = s.combat_condition_snapshot(id).unwrap().condition;
        for layer in [1, 2] {
            let old_damage = before.stats.max[layer] - before.now[layer];
            let new_damage = after.stats.max[layer] - after.now[layer];
            assert!((old_damage - new_damage).abs() < 1e-8);
        }
        assert!(after.shield_at >= before.shield_at);
    }
    assert_eq!(delivered_names(&mut s, &mut rx), vec![b"RemoveBalls".to_vec(), b"RemoveBalls".to_vec(), b"AddBalls".to_vec(), b"AddBalls".to_vec()]);
    s.take(old_id);
    let guard = s.begin_condition_transitions(&[target_id]).unwrap();
    let (token, state) = &guard.entries[0];
    let capsule = balls::piloted(&db, sde.clone(), &row(old_id, [4.0, 0.0, 0.0]), owner, 0.0).await.ok().unwrap();
    assert!(s.apply_committed_transition(transitions::CommittedTransition {
        old: Some(transitions::CommittedOld::Stored { id: target_id, generation: state.generation, token: *token }),
        target: transitions::CommittedTarget { id: old_id, sampled_position: [4.0, 0.0, 0.0], prepared: capsule, pilot: Some((7, owner)), existing: None },
        character: owner, expected: None, fresh: false, actor_gate: None,
    }));
    assert!(s.ball(target_id).is_none());
    assert_eq!(s.pilot_ball(owner).unwrap().id, old_id);
    assert_eq!(s.observers[&7].0, old_id);
    assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
    assert_eq!(delivered_names(&mut s, &mut rx), vec![b"RemoveBalls".to_vec(), b"AddBalls".to_vec()]);

    let free = balls::unpiloted(&db, sde.clone(), &row(target_id, [6.0, 0.0, 0.0])).await.ok().unwrap();
    free.install(&mut s, |s, ball| s.add_persistent_ship(ball));
    assert_eq!(s.ball(target_id).unwrap().char_id, None);
    assert_eq!(s.ball(target_id).unwrap().pos, [6.0, 0.0, 0.0]);
    assert_eq!(s.pilot_ball(owner).unwrap().id, old_id);
    assert_eq!(s.observers[&7].0, old_id);
    assert_eq!(delivered_names(&mut s, &mut rx), vec![b"AddBalls".to_vec()]);

    s.take(target_id);
    let snapshot_ball = s.sampled_ball(old_id).unwrap();
    let guard = s.begin_condition_transitions(&[old_id]).unwrap();
    let (token, state) = &guard.entries[0];
    let old = balls::unpiloted(&db, sde.clone(), &row(old_id, snapshot_ball.pos)).await.ok().unwrap();
    let target = balls::piloted(&db, sde.clone(), &row(target_id, [8.0, 0.0, 0.0]), owner, 0.0).await.ok().unwrap();
    assert!(s.apply_committed_transition(transitions::CommittedTransition {
        old: Some(transitions::CommittedOld::Retained { snapshot_ball, prepared: old, state: state.clone(), token: *token }),
        target: transitions::CommittedTarget { id: target_id, sampled_position: [8.0, 0.0, 0.0], prepared: target, pilot: Some((7, owner)), existing: None },
        character: owner, expected: None, fresh: false, actor_gate: None,
    }));
    assert_eq!(s.ball(old_id).unwrap().char_id, None);
    assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
    assert_eq!(s.observers[&7].0, target_id);
    assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
    assert_eq!(delivered_names(&mut s, &mut rx), vec![b"RemoveBalls".to_vec(), b"AddBalls".to_vec(), b"AddBalls".to_vec()]);
    }).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn stopped_actor_recovers_committed_boarding_from_loader_and_seeds() {
    use crate::dao::items::{boarding, creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let old_pos = [1.0e12, 1.0e12, 1.0e12];
    let target_pos = [1.0e12 + 2.0, 1.0e12, 1.0e12];
    let (account, owner, old_id, target_id) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await?.get("user_id");
            let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, solar_system_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, NULL, $2, 0) RETURNING character_id", &[&account, &system]).await?.get("character_id");
            let old = creation::positioned(tx, kind, owner, system, old_pos).await?;
            let target = creation::positioned(tx, kind, owner, system, target_pos).await?;
            tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &old]).await?;
            Ok::<_, crate::db::Error>((account, owner, old, target))
        }), |value, ()| async move { Ok(value) }).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let world = World::new(db.clone(), sde.clone(), 1);
        let row = |id, pos| FreeShip { item_id: id, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos };
        let old = balls::unpiloted(&db, sde.clone(), &row(old_id, old_pos)).await.ok().unwrap();
        let target = balls::piloted(&db, sde.clone(), &row(target_id, target_pos), owner, 0.0).await.ok().unwrap();
        world.system(system).await.unwrap();
        let original = balls::piloted(&db, sde.clone(), &row(old_id, old_pos), owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| original.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let snapshot = world.with(system, move |s| s.authorize_board(owner, old_id, target_id, 1.0e12))
            .await.unwrap().ok().unwrap();
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[old_id, target_id]).unwrap()).await.unwrap();
        let tokens = [transition.entries[0].0, transition.entries[1].0];
        let states = [transition.entries[0].1.clone(), transition.entries[1].1.clone()];
        let committed = db.transaction(
            |tx, _: &mut ()| Box::pin(async move {
                boarding::board_in_space(tx, owner, old_id, target_id, system, kind, kind, old_pos, target_pos).await
            }), |value, ()| async move { Ok(value) }).await.unwrap();
        assert!(committed);
        let selected: i64 = db.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap().get("ship_id");
        assert_eq!(selected, target_id);
        let restored = world.restore_persistent_ships(system).await.unwrap();
        assert!(restored.iter().any(|ship| ship.ball.id == old_id));
        assert!(!restored.iter().any(|ship| ship.ball.id == target_id));
        drop(restored);
        let generations = [states[0].generation, states[1].generation];
        let replacement = balls::piloted(&db, sde.clone(), &row(target_id, target_pos), owner, 0.0).await.ok().unwrap();
        let replacement_generation = world.with(system, move |s| {
            s.take(target_id).unwrap();
            replacement.install(s, |s, ball| s.add_ship(ball));
            assert_eq!(s.persistent_generation(old_id), Some(generations[0]));
            assert_eq!(s.combat_membership_generation(old_id), Some(generations[0]));
            assert_eq!(s.combat_phase(old_id), Some(crate::space::combat::CombatPhase::Transition { token: tokens[0] }));
            s.persistent_generation(target_id).unwrap()
        }).await.unwrap();
        let newer = infinity_core::session::OnlineSession { character_id: owner, user_id: None, session_id: 2,
            ship_id: Some(target_id), station_id: None, system_id: Some(system), corporation_id: Some(1),
            alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
        let (notify_tx, _notify_rx) = mpsc::unbounded_channel();
        world.register_pilot(owner, notify_tx);
        world.set_online_session(owner, Some(newer.clone()));
        assert_eq!(world.online_session(owner), Some(newer.clone()));
        assert!(world.apply_committed_transition(system, transitions::CommittedTransition {
            old: Some(transitions::CommittedOld::Retained { snapshot_ball: snapshot.old.clone(), prepared: old, state: states[0].clone(), token: tokens[0] }),
            target: transitions::CommittedTarget { id: target_id, sampled_position: snapshot.target.pos, prepared: target, pilot: Some((7, owner)), existing: Some((states[1].clone(), tokens[1])) },
            character: owner, expected: None, fresh: false, actor_gate: None,
        }).await.is_err());
        let old = balls::unpiloted(&db, sde.clone(), &row(old_id, old_pos)).await.ok().unwrap();
        let target = balls::piloted(&db, sde.clone(), &row(target_id, target_pos), owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| {
            assert!(s.ball(old_id).is_some());
            assert_eq!(s.combat_membership_generation(old_id), Some(generations[0]));
            assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
            assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
            assert_eq!(s.persistent_generation(target_id), Some(replacement_generation));
            assert_ne!(s.persistent_generation(target_id), Some(generations[1]));
        }).await.unwrap();
        assert_eq!(world.online_session(owner), Some(newer.clone()));
        assert_eq!(world.online_session(owner).unwrap().session_id, newer.session_id);

        // The actor's snapshot context owns this gate until the System is dropped.
        let handle = world.systems.lock().unwrap().remove(&system).unwrap();
        let stopped = Arc::downgrade(&handle.gate);
        drop(handle);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while stopped.upgrade().is_some() { tokio::task::yield_now().await; }
        }).await.expect("actor did not stop");
        assert!(world.apply_committed_transition(system, transitions::CommittedTransition {
            old: Some(transitions::CommittedOld::Retained { snapshot_ball: snapshot.old, prepared: old, state: states[0].clone(), token: tokens[0] }),
            target: transitions::CommittedTarget { id: target_id, sampled_position: snapshot.target.pos, prepared: target, pilot: Some((7, owner)), existing: Some((states[1].clone(), tokens[1])) },
            character: owner, expected: None, fresh: false, actor_gate: None,
        }).await.is_err());
        world.system(system).await.unwrap();
        let selected = balls::piloted(&db, sde.clone(), &row(target_id, target_pos), owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| selected.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        world.observe(system, 7, target_id, tx).await.unwrap();
        world.with(system, move |s| {
            assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
            assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
            assert_eq!(s.ball(old_id).unwrap().char_id, None);
            assert!(!s.ball(old_id).unwrap().interactive);
            assert!(s.persistent_generation(old_id).is_some());
            assert!(s.persistent_generation(target_id).is_some());
            assert_eq!(s.observers[&7].0, target_id);
        }).await.unwrap();
    }).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![old_id, target_id]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn dropped_dock_receipt_does_not_cancel_owned_checkpoint() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let (account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2) RETURNING character_id", &[&account, &system]).await?.get("character_id");
        let ship = creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
        tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &ship]).await?;
        Ok::<_, crate::db::Error>((account, owner, ship))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let result = crate::tests::catch_scenario(async {
        world.system(system).await.unwrap();
        world.remove_ball(system, ship).await;
        let row = FreeShip { item_id: ship, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = balls::piloted(&db, sde, &row, owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let (mut client, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        let connection = tokio::spawn(async move { let _ = connection.await; });
        let tx = client.transaction().await.unwrap();
        let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&ship]).await.unwrap();
        let market = crate::services::market::lock().await;
        let gate = world.lock_condition_snapshot(system).await.unwrap();
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        let w = world.clone();
        let d = db.clone();
        let caller = tokio::spawn(async move { w.checkpoint_dock(d, owner, ship, system, station, transition, market, gate).await });
        let scenario = crate::tests::catch_scenario(async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let row = db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.unwrap();
                    if row.get::<_, bool>("blocked") { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("dock worker did not reach the locked item");
        }).await;
        caller.abort();
        let _ = caller.await;
        tx.rollback().await.unwrap();
        drop(client);
        let _ = connection.await;
        if let Err(panic) = scenario {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let pending = world.with(system, move |s| matches!(s.combat_phase(ship),
                        Some(crate::space::combat::CombatPhase::Transition { .. }))).await.unwrap_or(false);
                    if !pending { break; }
                    tokio::task::yield_now().await;
                }
            }).await;
            std::panic::resume_unwind(panic);
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let row = db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
                let gone = world.with(system, move |s| s.ball(ship).is_none()).await.unwrap();
                if row.get::<_, i64>("location_id") == station && gone { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("owned dock did not settle after receipt cancellation");
    }).await;
    world.remove_ball(system, ship).await;
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        crate::services::tests::fixture::cleanup_progress(tx, &[owner], &[owner]).await
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn dock_reconciliation_restores_unchanged_and_retires_committed_membership() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let kind = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let (account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2) RETURNING character_id", &[&account, &system]).await?.get("character_id");
        let ship = creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
        tx.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner, &ship]).await?;
        Ok::<_, crate::db::Error>((account, owner, ship))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let result = crate::tests::catch_scenario(async {
        world.system(system).await.unwrap();
        world.remove_ball(system, ship).await;
        let row = FreeShip { item_id: ship, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = balls::piloted(&db, sde, &row, owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        let (token, generation) = (transition.entries[0].0, transition.entries[0].1.generation);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let plan = Arc::new(DockSettlement { world: Arc::downgrade(&world), db: db.clone(),
            character: owner, ship, system, station, token, generation, facts: transition.facts.clone(), expected: None, receipt: Mutex::new(Some(sender)) });
        plan.reconcile().await;
        assert!(receiver.await.unwrap().is_err());
        assert_eq!(world.with(system, move |s| s.combat_phase(ship)).await.unwrap(),
            Some(crate::space::combat::CombatPhase::Live));
        drop(transition);
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        let (token, generation) = (transition.entries[0].0, transition.entries[0].1.generation);
        db.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::dao::items::ship::dock(tx, ship, station, infinity_core::consts::FLAG_HANGAR).await?;
            crate::dao::characters::ship::dock(tx, owner, station, system).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async { Ok(()) }).await.unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let plan = Arc::new(DockSettlement { world: Arc::downgrade(&world), db: db.clone(),
            character: owner, ship, system, station, token, generation, facts: transition.facts.clone(), expected: None, receipt: Mutex::new(Some(sender)) });
        plan.reconcile().await;
        assert!(receiver.await.unwrap().is_ok());
        assert!(world.with(system, move |s| s.ball(ship).is_none()).await.unwrap());
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().get::<_, i64>("location_id"), station);
        drop(transition);
    }).await;
    world.remove_ball(system, ship).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn same_place_uncloaked_transfer_reconciles_committed_original_frame() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let destination = sde.map.systems().find(|row| row.solar_system_id != system && row.constellation_id.is_some() && row.region_id.is_some()).unwrap();
    let (dest, constellation, region) = (destination.solar_system_id, destination.constellation_id.unwrap(), destination.region_id.unwrap());
    let kind = sde.types.iter().find(|t| {
        if t.category_id != Some(CATEGORY_SHIP) { return false; }
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), t.type_id, Vec::new());
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        stats.max.iter().all(|v| v.is_finite() && *v > 10.0) && stats.resonance[0][0] > 0.0
    }).unwrap().type_id;
    let (account, owner, ship) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let owner: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,$2) RETURNING character_id", &[&account, &system]).await?.get("character_id");
        let ship = creation::positioned(tx, kind, owner, system, [0.0; 3]).await?;
        crate::dao::characters::selection::set_ship(tx, owner, ship).await?;
        Ok::<_, crate::db::Error>((account, owner, ship))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let result = crate::tests::catch_scenario(async {
        world.system(system).await.unwrap();
        let row = FreeShip { item_id: ship, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let prepared = balls::piloted(&db, sde.clone(), &row, owner, 0.0).await.ok().unwrap();
        world.with(system, move |s| {
            prepared.install(s, |s, ball| s.add_ship(ball));
            let generation = crate::space::targeting::next_generation();
            let source = s.prepare_combat_source(ship, Some(ship), Some(1), None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
            let damage = crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() };
            assert!(s.apply_damage(ship, [1.0, 0.0, 0.0, 0.0], damage, false).unwrap().unwrap().dealt > 0.0);
            s.finish_combat_attempt(&source);
            s.retire_combat_source(generation).unwrap();
        }).await.unwrap();
        let mut transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        let snapshot = transition.entries[0].1.clone();
        let token = transition.entries[0].0;
        let facts = transition.facts.clone().expect("native damage frame");
        let original = world.ball(system, ship).await.unwrap();
        transition.disarm();
        db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
            let snapshot = snapshot.clone();
            let facts = facts.clone();
            Box::pin(async move {
                crate::services::combat::checkpoint_conditions_in(tx, outbox, system, &[snapshot], Some(&facts)).await?;
                Ok::<_, CallError>(())
            })
        }, |(), _| async move { Ok(()) }).await.unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let plan = Arc::new(TransferSettlement {
            world: Arc::downgrade(&world), db: db.clone(), character: owner, ship, source: system, dest: system,
            original, position: [0.0; 3], prior_position: Mutex::new(Some([0.0; 3])),
            token, generation: snapshot.generation, snapshot, facts: Some(facts), scope: transition.scope.map(|(_, scope)| scope), expected: None, cloak_until: None,
            receipt: Mutex::new(Some(sender)),
        });
        let generation = plan.generation;
        let callback = plan.callback();
        assert!(world.with(system, move |s| s.mark_pending_transition(ship, token, generation, callback)).await.unwrap());
        plan.reconcile().await;
        assert!(receiver.await.unwrap().is_ok());
        assert!(world.ball(system, ship).await.is_some());
        // An in-flight source keeps the origin's receipt while the destination
        // imports the same character scope before publishing the transferred hull.
        world.system(dest).await.unwrap();
        let (pending_source, pending_generation, scope) = world.with(system, move |s| {
            s.close_combat_character_boundary(owner).unwrap();
            let scope = s.combat_character_scope(owner).unwrap();
            let generation = crate::space::targeting::next_generation();
            let source = s.prepare_combat_source(ship, Some(ship), Some(1), None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
            (source, generation, scope)
        }).await.unwrap();
        let mut transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        assert_eq!(transition.scope, Some((owner, scope)));
        let snapshot = transition.entries[0].1.clone();
        let token = transition.entries[0].0;
        let original = world.ball(system, ship).await.unwrap();
        transition.disarm();
        db.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::dao::items::ship::position(tx, ship, dest, [0.0; 3]).await?;
            crate::dao::characters::ship::location(tx, owner, dest, constellation, region).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async move { Ok(()) }).await.unwrap();
        let prepared = balls::piloted(&db, sde.clone(), &FreeShip { pos: [0.0; 3], ..row }, owner, 0.0).await.ok().unwrap();
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        let plan = TransferSettlement {
            world: Arc::downgrade(&world), db: db.clone(), character: owner, ship, source: system, dest,
            original, position: [0.0; 3], prior_position: Mutex::new(Some([0.0; 3])),
            token, generation: snapshot.generation, snapshot: snapshot.clone(), facts: transition.facts.clone(),
            scope: Some(scope), expected: None, cloak_until: None, receipt: Mutex::new(Some(sender)),
        };
        assert!(plan.publish(&world, prepared, snapshot).await);
        assert!(world.with(system, move |s| s.pilot_ball(owner).is_none()).await.unwrap());
        assert!(world.with(system, move |s| s.combat_character_scope(owner)).await.unwrap().is_some());
        let imported = world.with(dest, move |s| s.combat_character_scope(owner)).await.unwrap().unwrap();
        assert!(imported.at >= scope.at);
        world.with(system, move |s| {
            s.finish_combat_attempt(&pending_source);
            s.prune_combat_character_scopes();
            assert!(s.combat_character_scope(owner).is_some());
            s.retire_combat_source(pending_generation).unwrap();
            s.prune_combat_character_scopes();
            assert!(s.combat_character_scope(owner).is_none());
        }).await.unwrap();
        world.remove_ball(dest, ship).await;
    }).await;
    world.remove_ball(system, ship).await;
    world.remove_ball(dest, ship).await;
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$1)", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE subject_character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
