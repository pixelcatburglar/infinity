#[path = "tests/governance.rs"]
mod governance;
#[path = "tests/loaded.rs"]
mod loaded;
#[path = "tests/helpers.rs"]
mod helpers;
use helpers::{ball, delivered, expected_add};
use super::*;

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
    assert!(s.authorize_board(1, 10, 20, 10.0).is_ok());
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
    let (completed, mut done) = tokio::sync::oneshot::channel();
    assert!(s.observe(7, old_id, tx, completed));
    s.apply_scene_prefix(crate::space::collision::enabled());
    assert_eq!(done.try_recv(), Ok(true));
    crate::space::system::tests::events::initial_state(&mut rx);
    crate::space::system::tests::events::fleet_snapshot(&mut rx);
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
    assert_eq!(s.actions.len(), queued + 4); // Two exits and two births; unified projector handles entries.
    assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
    assert_eq!(s.ball(old_id).unwrap().char_id, None);
    assert!(!s.ball(old_id).unwrap().interactive);
    assert_eq!(s.observer_ego(7), Some(target_id));
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
    let before = crate::client::filetime_now();
    let stamp = s.current_time();
    s.apply_scene_prefix(crate::space::collision::enabled());
    let mut actual = Vec::new();
    while let Ok(delivery) = rx.try_recv() {
        match delivery {
            crate::space::SceneDelivery::Destiny(events) => actual.extend(events),
            crate::space::SceneDelivery::Notify { event, .. } => assert_eq!(event, "OnFleetStateChange"),
        }
    }
    let names: Vec<&[u8]> = actual.iter().map(|event| {
        let Value::Tuple(parts) = event else { panic!("event tuple") };
        let Value::Tuple(call) = &parts[1] else { panic!("event call") };
        let Value::Bytes(name) = &call[0] else { panic!("event name") };
        name.as_slice()
    }).collect();
    assert_eq!(names, vec![b"RemoveBalls".as_slice(), b"RemoveBalls".as_slice(), b"AddBalls".as_slice()]);
    let collisions = crate::space::collision::enabled();
    let mut stream = vec![1u8]; // DESTINY_BALLS kind byte
    stream.extend_from_slice(&(stamp as i32).to_le_bytes());
    s.ball(old_id).unwrap().write(&mut stream, collisions);
    s.ball(target_id).unwrap().write(&mut stream, collisions);
    let slims = Value::list([s.ball(old_id).unwrap().slim_item(), s.ball(target_id).unwrap().slim_item()]);
    let damage = Value::Dict([old_id, target_id].iter().filter_map(|id| {
        let ball = s.ball(*id).unwrap();
        ball.free.then(|| (Value::Int(*id), ball.damage_state()))
    }).collect());
    let mut expected = vec![crate::space::event(stamp, "AddBalls", vec![Value::tuple([Value::str(stream), slims, damage])])];
    crate::space::system::tests::events::reuse_checked_damage_time(&actual[2], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(vec![actual[2].clone()], expected, "the shared dirty-bubble Add carries both entities exactly once");
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
    assert_eq!(s.observer_ego(7), Some(old_id));
    assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
    let before = crate::client::filetime_now();
    let stamp = s.current_time();
    let actual = delivered(&mut s, &mut rx);
    let mut expected = vec![
        crate::space::event(stamp, "RemoveBalls", vec![Value::list([Value::Int(old_id)])]),
        crate::space::event(stamp, "RemoveBalls", vec![Value::list([Value::Int(target_id)])]),
        expected_add(&s, stamp, &[old_id]),
    ];
    crate::space::system::tests::events::reuse_checked_damage_time(&actual[2], &mut expected[2], before..=crate::client::filetime_now());
    assert_eq!(actual, expected, "both real departures precede the capsule Add");

    let free = balls::unpiloted(&db, sde.clone(), &row(target_id, [6.0, 0.0, 0.0])).await.ok().unwrap();
    free.install(&mut s, |s, ball| s.add_persistent_ship(ball));
    assert_eq!(s.ball(target_id).unwrap().char_id, None);
    assert_eq!(s.ball(target_id).unwrap().pos, [6.0, 0.0, 0.0]);
    assert_eq!(s.pilot_ball(owner).unwrap().id, old_id);
    assert_eq!(s.observer_ego(7), Some(old_id));
    let before = crate::client::filetime_now();
    let stamp = s.current_time();
    let actual = delivered(&mut s, &mut rx);
    let mut expected = vec![expected_add(&s, stamp, &[target_id])];
    crate::space::system::tests::events::reuse_checked_damage_time(&actual[0], &mut expected[0], before..=crate::client::filetime_now());
    assert_eq!(actual, expected, "the restored free hull is one current Add");

    s.take(target_id);
    s.command(old_id, |ball, _, _| { ball.vel = [0.0, 20.0, 0.0]; Vec::new() });
    let advance = |s: &mut System| s.tick(s.now_ms().saturating_add(MILLISECONDS_PER_SECOND - s.now_ms().rem_euclid(MILLISECONDS_PER_SECOND)));
    advance(&mut s);
    advance(&mut s);
    let snapshot_ball = s.sampled_ball(old_id).unwrap();
    let sampled_velocity = snapshot_ball.vel;
    assert!(sampled_velocity[1] > 0.0);
    let guard = s.begin_condition_transitions(&[old_id]).unwrap();
    let (token, state) = &guard.entries[0];
    let old = balls::unpiloted(&db, sde.clone(), &row(old_id, snapshot_ball.pos)).await.ok().unwrap();
    assert_eq!(old.ball.vel, [0.0; 3]);
    let target = balls::piloted(&db, sde.clone(), &row(target_id, [8.0, 0.0, 0.0]), owner, 0.0).await.ok().unwrap();
    assert!(s.apply_committed_transition(transitions::CommittedTransition {
        old: Some(transitions::CommittedOld::Retained { snapshot_ball, prepared: old, state: state.clone(), token: *token }),
        target: transitions::CommittedTarget { id: target_id, sampled_position: [8.0, 0.0, 0.0], prepared: target, pilot: Some((7, owner)), existing: None },
        character: owner, expected: None, fresh: false, actor_gate: None,
    }));
    let retained = s.ball(old_id).unwrap();
    assert_eq!((retained.mode, retained.vel, retained.char_id), (MODE_STOP, sampled_velocity, None));
    advance(&mut s);
    let decayed = s.ball(old_id).unwrap().vel[1];
    assert!(0.0 < decayed && decayed < sampled_velocity[1], "retained hull must decelerate through native stop evolution, got {decayed}");
    assert_eq!(s.ball(target_id).unwrap().vel, [0.0; 3]);
    assert_eq!(s.pilot_ball(owner).unwrap().id, target_id);
    assert_eq!(s.observer_ego(7), Some(target_id));
    assert_eq!(s.pilots().filter(|b| b.char_id == Some(owner)).count(), 1);
    let before = crate::client::filetime_now();
    let stamp = s.current_time();
    let actual = delivered(&mut s, &mut rx);
    let mut expected = vec![
        crate::space::event(stamp, "RemoveBalls", vec![Value::list([Value::Int(target_id)])]),
        crate::space::event(stamp, "RemoveBalls", vec![Value::list([Value::Int(old_id)])]),
        expected_add(&s, stamp, &[old_id, target_id]),
    ];
    crate::space::system::tests::events::reuse_checked_damage_time(&actual[2], &mut expected[2], before..=crate::client::filetime_now());
    assert_eq!(actual, expected, "both real departures precede the shared current Add");
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
        crate::space::session_change::test_support::register_elapsed(&world, owner, notify_tx);
        world.set_online_session(owner, Some(newer.clone()));
        assert_eq!(world.online_session(owner), Some(newer.clone()));
        let elapsed = crate::space::session_change::ship_change_deadline(0);
        assert!(!world.ship_change_ready(owner, newer.session_id, elapsed - 1));
        assert!(world.ship_change_ready(owner, newer.session_id, crate::client::filetime_now()));
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
            assert_eq!(s.observer_ego(7), Some(target_id));
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
    let station_row = sde.map.stations().find(|row| row.solar_system_id.is_some()
        && row.constellation_id.is_some() && row.region_id.is_some()).unwrap();
    let destination = station::Destination { station_id: station_row.station_id, system_id: station_row.solar_system_id.unwrap(),
        constellation_id: station_row.constellation_id.unwrap(), region_id: station_row.region_id.unwrap() };
    let station = destination.station_id;
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
        let admission = world.admission(system).unwrap();
        let market = crate::services::market::lock().await;
        let gate = world.lock_condition_snapshot(system).await.unwrap();
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        let w = world.clone();
        let d = db.clone();
        let caller = tokio::spawn(async move { w.checkpoint_dock(d, owner, ship, system, destination, transition, market, gate, admission).await });
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
    let station_row = sde.map.stations().find(|row| row.solar_system_id.is_some()
        && row.solar_system_id != Some(system) && row.constellation_id.is_some() && row.region_id.is_some()).unwrap();
    let destination = station::Destination { station_id: station_row.station_id, system_id: station_row.solar_system_id.unwrap(),
        constellation_id: station_row.constellation_id.unwrap(), region_id: station_row.region_id.unwrap() };
    let station = destination.station_id;
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
            character: owner, ship, source_system: system, destination, token, generation, facts: transition.facts.clone(), bounties: None, expected: None,
            admission: world.admission(system).unwrap(), receipt: Mutex::new(Some(sender)) });
        plan.reconcile().await;
        assert!(receiver.await.unwrap().is_err());
        assert_eq!(world.with(system, move |s| s.combat_phase(ship)).await.unwrap(),
            Some(crate::space::combat::CombatPhase::Live));
        drop(transition);
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        let (token, generation) = (transition.entries[0].0, transition.entries[0].1.generation);
        db.transaction(|tx, _: &mut ()| Box::pin(async move {
            tx.execute("UPDATE game.items SET location_id=$2, flag_id=$3, x=NULL, y=NULL, z=NULL WHERE item_id=$1",
                &[&ship, &destination.station_id, &infinity_core::consts::FLAG_HANGAR]).await?;
            tx.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3, constellation_id=$4, region_id=$5 WHERE character_id=$1",
                &[&owner, &destination.station_id, &destination.system_id, &destination.constellation_id, &destination.region_id]).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async { Ok(()) }).await.unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let plan = Arc::new(DockSettlement { world: Arc::downgrade(&world), db: db.clone(),
            character: owner, ship, source_system: system, destination, token, generation, facts: transition.facts.clone(), bounties: None, expected: None,
            admission: world.admission(system).unwrap(), receipt: Mutex::new(Some(sender)) });
        plan.reconcile().await;
        assert!(receiver.await.unwrap().is_ok());
        assert!(world.with(system, move |s| s.ball(ship).is_none()).await.unwrap());
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().get::<_, i64>("location_id"), station);
        // Cross-system committed proof: the character row carries the full
        // destination ancestry, not just the hull's root location.
        let row = db.query_one("SELECT station_id, solar_system_id, constellation_id, region_id FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
        assert_eq!(row.get::<_, Option<i64>>("station_id"), Some(station));
        assert_eq!(row.get::<_, i64>("solar_system_id"), destination.system_id);
        assert_eq!(row.get::<_, i64>("constellation_id"), destination.constellation_id);
        assert_eq!(row.get::<_, i64>("region_id"), destination.region_id);
        assert_ne!(destination.system_id, system);
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
            token, generation: snapshot.generation, snapshot, facts: Some(facts), bounties: None, scope: transition.scope.map(|(_, scope)| scope), expected: None, cloak_until: None,
            admission: (world.admission(system).unwrap(), None),
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
            token, generation: snapshot.generation, snapshot: snapshot.clone(), facts: transition.facts.clone(), bounties: None,
            scope: Some(scope), expected: None, cloak_until: None, admission: (world.admission(system).unwrap(), Some(world.admission(dest).unwrap())), receipt: Mutex::new(Some(sender)),
        };
        assert!(plan.publish(&world, prepared, snapshot, Vec::new()).await);
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


fn bounty_call_err(e: crate::services::CallError) -> anyhow::Error {
    match e {
        crate::services::CallError::Internal(e) => e,
        crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn bounty_boundary_receipts_cover_commit_rollback_and_unknown() {
    use crate::dao::items::{creation, space_ships::FreeShip};
    use crate::services::ship::balls;
    use crate::services::bounty::npc::checkpoint::CheckpointOutcome;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let station_row = sde.map.stations().find(|row| row.solar_system_id.is_some()
        && row.constellation_id.is_some() && row.region_id.is_some()).unwrap();
    let destination = station::Destination { station_id: station_row.station_id, system_id: station_row.solar_system_id.unwrap(),
        constellation_id: station_row.constellation_id.unwrap(), region_id: station_row.region_id.unwrap() };
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
        // commit: a dirty bucket rides the dock's own transaction and proves Durable.
        world.with(system, move |s| s.add_npc_bounty(owner, 42, crate::isk::IskAmount::from_cents(777).unwrap(), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), crate::client::filetime_now())).await.unwrap().unwrap();
        let admission = world.admission(system).unwrap();
        let market = crate::services::market::lock().await;
        let gate = world.lock_condition_snapshot(system).await.unwrap();
        let transition = world.with(system, move |s| s.begin_condition_transitions(&[ship]).unwrap()).await.unwrap();
        world.checkpoint_dock(db.clone(), owner, ship, system, destination.clone(), transition, market, gate, admission).await.unwrap();
        let rows = crate::dao::npc_bounties::for_character(&db, owner).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].amount, rows[0].kill_count, rows[0].legacy_kill_count, rows[0].security_gain), (crate::isk::IskAmount::from_cents(777).unwrap(), 1, 0, crate::npc_security::SecurityGain::for_kill(crate::config::game().crimewatch.security_gain_npc_kill, crate::npc_security::FrozenMultiplier::new(1.0).unwrap())));
        assert!(world.with(system, move |s| s.bounty_pending_receipts(Some(owner)).is_empty() && !s.bounty_checkpoint_pending()).await.unwrap());
        // rollback: a write transaction that fails leaves the receipt Unchanged
        // and the dirty bucket intact.
        world.with(system, move |s| s.add_npc_bounty(owner, 42, crate::isk::IskAmount::from_cents(50).unwrap(), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), crate::client::filetime_now() + 1)).await.unwrap().unwrap();
        let receipt = world.with(system, move |s| s.capture_npc_bounties(Some(owner))).await.unwrap().unwrap().unwrap();
        let failed = db.transaction(|tx, _: &mut ()| {
            let receipt = receipt.clone();
            Box::pin(async move {
                receipt.write_in(tx).await.map_err(|e| crate::db::Error::Progress(bounty_call_err(e)))?;
                Err::<_, crate::db::Error>(crate::db::Error::Progress(anyhow::anyhow!("forced rollback")))
            })
        }, |value: (), ()| async move { Ok(value) }).await;
        assert!(failed.is_err());
        let outcome = crate::services::bounty::npc::resolve_receipt(&db, &receipt).await.unwrap();
        assert_eq!(outcome, CheckpointOutcome::Unchanged);
        receipt.settle(&world, outcome).await.unwrap();
        assert!(world.with(system, |s| s.bounty_checkpoint_pending()).await.unwrap());
        world.flush_positions().await.unwrap();
        assert_eq!(crate::dao::npc_bounties::for_character(&db, owner).await.unwrap()[0].kill_count, 2);
        // unknown: an outside change between failed write and resolve is
        // neither Durable nor Unchanged, and the receipt stays owned.
        world.with(system, move |s| s.add_npc_bounty(owner, 42, crate::isk::IskAmount::from_cents(1).unwrap(), crate::npc_security::FrozenMultiplier::new(1.0).unwrap(), crate::client::filetime_now() + 2)).await.unwrap().unwrap();
        let receipt = world.with(system, move |s| s.capture_npc_bounties(Some(owner))).await.unwrap().unwrap().unwrap();
        let failed = db.transaction(|tx, _: &mut ()| {
            let receipt = receipt.clone();
            Box::pin(async move {
                receipt.write_in(tx).await.map_err(|e| crate::db::Error::Progress(bounty_call_err(e)))?;
                Err::<_, crate::db::Error>(crate::db::Error::Progress(anyhow::anyhow!("forced rollback")))
            })
        }, |value: (), ()| async move { Ok(value) }).await;
        assert!(failed.is_err());
        db.execute("UPDATE game.npc_bounty_pools SET amount=amount+'1.00'::numeric WHERE character_id=$1", &[&owner]).await.unwrap();
        let outcome = crate::services::bounty::npc::resolve_receipt(&db, &receipt).await.unwrap();
        assert_eq!(outcome, CheckpointOutcome::Unknown);
        receipt.settle(&world, outcome).await.unwrap();
        assert_eq!(world.with(system, move |s| s.bounty_pending_receipts(Some(owner))).await.unwrap().len(), 1);
    }).await;
    db.execute("DELETE FROM game.npc_bounty_pools WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id LIKE 'npc-bounty:' || $1::text || ':%'", &[&owner.to_string()]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
