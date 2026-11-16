use marshal::Value;
use std::sync::Arc;
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn headless_leave_saves_owned_ship_before_unobserving_and_clears_presence() {
    use infinity_core::{consts::{TYPE_CAPSULE, dogma::{ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_SHIELD_CHARGE}}, session::OnlineSession};
    use crate::{config::deployment::DatabaseConfig, dao::{characters, items}, db::Db, sde::Sde, services::{combat, identity::Identity, ship}, space::World};
    crate::tests::init_config();
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system_row = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    let (system, constellation, region) = (system_row.solar_system_id, system_row.constellation_id.unwrap(), system_row.region_id.unwrap());
    let (account, character, ship_id, peer) = db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES (nextval('game.accounts_user_id_seq')::text,''::bytea,0) RETURNING user_id", &[]).await?.get("user_id");
        let character: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await?.get("character_id");
        let peer: i64 = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,station_id,solar_system_id,constellation_id,region_id) VALUES ($1,nextval('game.characters_character_id_seq')::text,1,1,1,1,NULL,$2,$3,$4) RETURNING character_id", &[&account, &system, &constellation, &region]).await?.get("character_id");
        let ship = items::creation::positioned(tx, TYPE_CAPSULE, character, system, [1.0, 2.0, 3.0]).await?;
        characters::selection::set_ship(tx, character, ship).await?;
        Ok::<_, crate::db::Error>((account, character, ship, peer))
    }), |value, ()| async move { Ok(value) }).await.unwrap();
    let peer_ship = items::creation::positioned(&db, TYPE_CAPSULE, peer, system, [1.0e15, 2.0e15, 3.0e15]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let cfg = crate::config::Config::default();
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let test_db = db.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
        assert_eq!(items::dogma::save_conditions(&test_db, system, &ids, &[(ship_id, [0.0, 1.0, 0.0], 0)]).await.unwrap(), vec![ship_id]);
        let row = items::space_ships::FreeShip { item_id: ship_id, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: 0, pos: [1.0e15, 2.0e15, 3.0e15] };
        let prepared = ship::balls::piloted(&test_db, sde.clone(), &row, character, 0.0).await.ok().unwrap();
        test_world.system(system).await.unwrap();
        test_world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        assert!((test_world.with(system, move |s| s.ship_condition(ship_id)).await.flatten().unwrap().1 - 1.0).abs() < 1e-9);
        assert!(test_world.with(system, move |s| s.repair_condition(ship_id, 1, 1.0, crate::space::combat::RepairSource::Local)).await.unwrap().unwrap());
        // A dirty loaded absolute rides the logout checkpoint like a real hull's.
        test_db.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1,27,$2,5)", &[&ship_id, &TYPE_CAPSULE]).await.unwrap();
        test_world.with(system, move |s| {
            let membership = s.combat_membership_generation(ship_id).unwrap();
            assert!(s.publish_loaded_absolute(ship_id, membership, vec![crate::space::combat::loaded::LoadedSeed {
                module: ship_id + 1, flag: 27, type_id: Some(TYPE_CAPSULE), quantity: 5, pending: false }]));
            let permit = s.hold_loaded_use(ship_id, ship_id + 1).unwrap();
            assert!(s.consume_loaded(permit).is_some());
        }).await.unwrap();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        combat::state().active.insert(ship_id, combat::ActiveModule {
            char_id: character, stop: stopped.clone(), forced_inactive: false,
        });
        let identity = Identity { char_id: character, ship_id: Some(ship_id), corp_id: 1, station_id: None, system_id: Some(system) };
        let (notify_tx, _notify_rx) = tokio::sync::mpsc::unbounded_channel();
        super::online(&identity, &test_world, &chat, notify_tx, None, OnlineSession {
            character_id: character, user_id: None, session_id: 0, ship_id: None, station_id: None, system_id: Some(system), corporation_id: Some(1),
            alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
        });
        let position = test_world.with(system, move |s| s.pilot_ball(character).unwrap().pos).await.unwrap();
        assert_ne!(position, [1.0, 2.0, 3.0]);
        let (boundary_tx, boundary_rx) = tokio::sync::oneshot::channel();
        let callback_world = test_world.clone();
        super::leave(&identity, Some(account), &test_db, &test_world, &chat, &sde, Some(system), None, move || {
            assert!(callback_world.online_session(character).is_some());
            callback_world.post(system, move |s| {
                let _ = boundary_tx.send(s.pilot_ball(character).is_none() && s.ball(ship_id).is_none());
            });
        }).await.unwrap();
        let events = crate::db::outbox::tests::events(&test_db, "CharacterLoggedOff", character).await;
        assert!(matches!(events.as_slice(), [crate::lua::events::GameEvent::CharacterLoggedOff { user_id, char_id }] if *user_id == account && *char_id == character));
        assert!(boundary_rx.await.unwrap());
        assert!(stopped.load(std::sync::atomic::Ordering::Relaxed));
        combat::state().active.remove(&ship_id);
        assert!(!test_world.is_online(character));
        assert!(test_world.online_session(character).is_none());
        let saved = test_db.query_one("SELECT x,y,z FROM game.items WHERE item_id=$1", &[&ship_id]).await.unwrap();
        assert_eq!([saved.get::<_, f64>("x"), saved.get("y"), saved.get("z")], position);
        let rows = items::dogma::overrides(&test_db, ship_id).await.unwrap();
        for id in ids { assert!(rows.iter().any(|row| row.attribute_id == id)); }
        assert_eq!(rows.iter().find(|row| row.attribute_id == ATTR_ARMOR_DAMAGE).unwrap().value, 0.0);
        assert_eq!(items::charges::loaded(&test_db, ship_id, 27).await.unwrap().unwrap().quantity, 4,
            "the logout checkpoint settled the dirty absolute");
        // Fixture-local targeting attributes keep the real lock completing
        // quickly; the peer hull sits outside both collision radii in range.
        for (attribute, value) in [(ATTR_SCAN_RESOLUTION, 10_000.0), (ATTR_MAX_TARGET_RANGE, 1.0e9)] {
            assert!(items::dogma::set_override(&test_db, ship_id, attribute, value).await.unwrap());
        }
        let prepared = ship::balls::piloted(&test_db, sde.clone(), &items::space_ships::FreeShip {
            item_id: ship_id, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: 0, pos: position,
        }, character, 0.0).await.ok().unwrap();
        test_world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let old_session = OnlineSession { character_id: character, user_id: None, session_id: 10, ship_id: Some(ship_id), station_id: None, system_id: Some(system), corporation_id: Some(1), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
        let (old_tx, _old_rx) = tokio::sync::mpsc::unbounded_channel();
        super::online(&identity, &test_world, &chat, old_tx, None, old_session.clone());
        let (ctx_tx, _ctx_rx) = tokio::sync::mpsc::unbounded_channel();
        let cx = combat::Ctx::from_identity(&identity, &test_db, &test_world, Some(system), ctx_tx).unwrap();
        let spacing = 2.0 * f64::from(ship::balls::radius(&sde, TYPE_CAPSULE).unwrap()) + 1_000.0;
        let peer_prepared = ship::balls::piloted(&test_db, sde.clone(), &items::space_ships::FreeShip { item_id: peer_ship, type_id: TYPE_CAPSULE, owner_id: peer, corporation_id: 1, alliance_id: 0, pos: [position[0] + spacing, position[1], position[2]] }, peer, 0.0).await.ok().unwrap();
        test_world.with(system, move |s| peer_prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let (peer_tx, mut peer_rx) = tokio::sync::mpsc::unbounded_channel();
        let peer_identity = Identity { char_id: peer, ship_id: Some(peer_ship), corp_id: 1, station_id: None, system_id: Some(system) };
        super::online(&peer_identity, &test_world, &chat, peer_tx, None, OnlineSession { character_id: peer, session_id: 100, ship_id: Some(peer_ship), ..old_session.clone() });
        let departed_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        combat::state().active.insert(ship_id, combat::ActiveModule { char_id: character, stop: departed_stop.clone(), forced_inactive: false });
        combat::targeting_charges::add_target_ctx(&cx, peer_ship, false).await.unwrap();
        for _ in 0..200 {
            if combat::targeting_charges::has_target(character, peer_ship) { break; }
            assert!(test_world.after(system, 50).await);
        }
        assert!(combat::targeting_charges::has_target(character, peer_ship));
        test_world.with(system, |_| ()).await.unwrap();
        let mut heard_add = false;
        while let Ok((event, args)) = peer_rx.try_recv() { heard_add |= event == "OnTarget" && args == vec![Value::str("otheradd"), Value::Int(ship_id)]; }
        assert!(heard_add);
        let mut transition = test_world.with(system, move |s| s.begin_condition_transitions(&[ship_id]).unwrap()).await.unwrap();
        let (token, snapshot) = transition.entries[0].clone();
        transition.disarm();
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
        let cleanup = Arc::new(super::LogoutCleanup {
            identity, db: test_db.clone(), world: Arc::downgrade(&test_world), chat: Arc::downgrade(&chat),
            sde: sde.clone(), expected: Some(old_session.clone()), space_ship: std::sync::Mutex::new(Some((system, ship_id))),
            unobserve: std::sync::Mutex::new(None), receipt: std::sync::Mutex::new(Some(receipt_tx)),
        });
        let committed_position = [position[0] + 1000.0, position[1], position[2]];
        let plan = Arc::new(super::LogoutSettlement {
            cleanup, system, ship: ship_id, token, snapshot, facts: transition.facts.clone(), position: committed_position,
            before: std::sync::Mutex::new(None), bounties: None,
            logoff_possible: false, skipped_logoff: std::sync::atomic::AtomicBool::new(false), before_logoff: std::sync::Mutex::new(None),
        });
        let (_, prior_pos, prior_rows) = plan.read_checkpoint(&test_db).await.unwrap();
        *plan.before.lock().unwrap() = Some((prior_pos, prior_rows));
        let generation = plan.snapshot.generation;
        let callback = plan.callback();
        assert!(test_world.with(system, move |s| s.mark_pending_transition(ship_id, token, generation, callback)).await.unwrap());
        let live_condition = test_world.with(system, move |s| s.ship_condition(ship_id)).await.unwrap();
        plan.clone().reconcile().await;
        assert_eq!(receipt_rx.await.unwrap().unwrap_err(), "logout checkpoint did not commit");
        assert_eq!(test_world.with(system, move |s| (s.combat_phase(ship_id), s.combat_membership_generation(ship_id))).await.unwrap(),
            (Some(crate::space::combat::CombatPhase::Live), Some(generation)));
        assert!(!test_world.is_online(character));
        assert!(test_world.online_session(character).is_none());
        assert_eq!(test_world.with(system, move |s| s.ball(ship_id).map(|ball| ball.pos)).await.unwrap(), Some(position));
        assert_eq!(plan.read_checkpoint(&test_db).await.unwrap(), (true, prior_pos, prior_rows));
        let after_condition = test_world.with(system, move |s| s.ship_condition(ship_id)).await.unwrap().unwrap();
        // Shield recharge is monotonic; armor and hull must be exactly retained.
        let live = live_condition.unwrap();
        assert!(after_condition.0 >= live.0 && after_condition.1 == live.1 && after_condition.2 == live.2);
        assert!(departed_stop.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!combat::targeting_charges::has_target(character, peer_ship));
        let mut otherlost = 0;
        while let Ok((event, args)) = peer_rx.try_recv() { otherlost += usize::from(event == "OnTarget" && args == vec![Value::str("otherlost"), Value::Int(ship_id), Value::None]); }
        assert_eq!(otherlost, 1);
        combat::targeting_charges::add_target_ctx(&cx, peer_ship, false).await.unwrap();
        for _ in 0..200 {
            if combat::targeting_charges::has_target(character, peer_ship) { break; }
            assert!(test_world.after(system, 50).await);
        }
        assert!(combat::targeting_charges::has_target(character, peer_ship));
        let (old_tx, _old_rx) = tokio::sync::mpsc::unbounded_channel();
        super::online(&identity, &test_world, &chat, old_tx, None, old_session.clone());
        let mut transition = test_world.with(system, move |s| s.begin_condition_transitions(&[ship_id]).unwrap()).await.unwrap();
        let (token, snapshot) = transition.entries[0].clone();
        transition.disarm();
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
        *plan.cleanup.receipt.lock().unwrap() = Some(receipt_tx);
        let plan = Arc::new(super::LogoutSettlement {
            cleanup: plan.cleanup.clone(), system, ship: ship_id, token, snapshot, facts: transition.facts.clone(), position: committed_position,
            before: std::sync::Mutex::new(Some((prior_pos, prior_rows))), bounties: None,
            logoff_possible: true, skipped_logoff: std::sync::atomic::AtomicBool::new(false),
            before_logoff: std::sync::Mutex::new(Some(characters::selection::logout_timestamps(&test_db, character).await.unwrap())),
        });
        let timestamps_before = characters::selection::logout_timestamps(&test_db, character).await.unwrap();
        let generation = plan.snapshot.generation;
        let callback = plan.callback();
        assert!(test_world.with(system, move |s| s.mark_pending_transition(ship_id, token, generation, callback)).await.unwrap());
        let mut replacement = old_session.clone();
        replacement.session_id += 1;
        let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        super::online(&identity, &test_world, &chat, replacement_tx, None, replacement.clone());
        // The replacement session owns fresh combat state: a running module
        // flag and the re-locked peer target must outlive the stale cleanup.
        let replacement_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        combat::state().active.insert(peer_ship, combat::ActiveModule { char_id: character, stop: replacement_stop.clone(), forced_inactive: false });
        test_db.transaction(|tx, _: &mut ()| {
            let plan = plan.clone();
            let world = test_world.clone();
            Box::pin(async move {
                plan.skipped_logoff.store(false, std::sync::atomic::Ordering::SeqCst);
                assert!(plan.logoff_user(Some(account), &world).is_none());
                assert_eq!(items::ship::position(tx, ship_id, system, committed_position).await?, 1);
                Ok::<_, crate::db::Error>(())
            })
        }, |(), ()| async move { Ok(()) }).await.unwrap();
        assert_eq!(characters::selection::logout_timestamps(&test_db, character).await.unwrap(), timestamps_before);
        plan.reconcile().await;
        assert!(receipt_rx.await.unwrap().is_ok());
        assert_eq!(test_world.online_session(character), Some(replacement));
        assert!(test_world.ball(system, ship_id).await.is_none());
        assert!(test_world.unregister_pilot_generation(character, None));
        // The stale cleanup matched no session: replacement combat state and
        // the peer's announced lock are untouched.
        assert!(!replacement_stop.load(std::sync::atomic::Ordering::Relaxed));
        assert!(combat::targeting_charges::has_target(character, peer_ship));
        let mut otherlost = 0;
        while let Ok((event, args)) = peer_rx.try_recv() { otherlost += usize::from(event == "OnTarget" && args == vec![Value::str("otherlost"), Value::Int(ship_id), Value::None]); }
        assert_eq!(otherlost, 0);
        // A previously journaled frame must permit a stationary headless logout
        // only after both the source receipts and this hull's HP are durable.
        let prepared = ship::balls::piloted(&test_db, sde.clone(), &items::space_ships::FreeShip {
            item_id: ship_id, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: 0, pos: committed_position,
        }, character, 0.0).await.ok().unwrap();
        test_world.with(system, move |s| {
            prepared.install(s, |s, ball| s.add_ship(ball));
            let source = s.prepare_combat_source(ship_id, Some(ship_id), Some(1), None,
                crate::lua::events::resources::FactOrigin::Natural, crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap();
            let damage = crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() };
            assert!(s.apply_damage(ship_id, [1.0, 0.0, 0.0, 0.0], damage, false).unwrap().unwrap().dealt > 0.0);
        }).await.unwrap();
        let mut transition = test_world.with(system, move |s| s.begin_condition_transitions(&[ship_id]).unwrap()).await.unwrap();
        let (token, snapshot) = transition.entries[0].clone();
        let facts = transition.facts.clone().expect("actual damage fact");
        transition.disarm();
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
        let cleanup = Arc::new(super::LogoutCleanup {
            identity, db: test_db.clone(), world: Arc::downgrade(&test_world), chat: Arc::downgrade(&chat),
            sde: sde.clone(), expected: None, space_ship: std::sync::Mutex::new(Some((system, ship_id))), unobserve: std::sync::Mutex::new(None),
            receipt: std::sync::Mutex::new(Some(receipt_tx)),
        });
        let plan = Arc::new(super::LogoutSettlement {
            cleanup, system, ship: ship_id, token, snapshot: snapshot.clone(), facts: Some(facts.clone()),
            position: committed_position, before: std::sync::Mutex::new(None), bounties: None,
            logoff_possible: false, skipped_logoff: std::sync::atomic::AtomicBool::new(false), before_logoff: std::sync::Mutex::new(None),
        });
        let (_, pos, rows) = plan.read_checkpoint(&test_db).await.unwrap();
        *plan.before.lock().unwrap() = Some((pos, rows));
        test_db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
            let snapshot = snapshot.clone();
            let facts = facts.clone();
            Box::pin(async move {
                super::super::combat::checkpoint_conditions_in(tx, outbox, system, &[snapshot], Some(&facts)).await?;
                Ok::<_, crate::services::CallError>(())
            })
        }, |(), _| async move { Ok(()) }).await.unwrap();
        let generation = plan.snapshot.generation;
        let callback = plan.callback();
        assert!(test_world.with(system, move |s| s.mark_pending_transition(ship_id, token, generation, callback)).await.unwrap());
        plan.reconcile().await;
        assert!(receipt_rx.await.unwrap().is_ok());
        assert!(test_world.ball(system, ship_id).await.is_none());

        // A real logout receipt cannot acknowledge collateral HP that was not
        // written because the persistent item no longer matches this location.
        let collateral = items::creation::positioned(&test_db, TYPE_CAPSULE, character, 0, [0.0; 3]).await.unwrap();
        let own = ship::balls::piloted(&test_db, sde.clone(), &items::space_ships::FreeShip {
            item_id: ship_id, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: 0, pos: committed_position,
        }, character, 0.0).await.ok().unwrap();
        let other = ship::balls::unpiloted(&test_db, sde.clone(), &items::space_ships::FreeShip {
            item_id: collateral, type_id: TYPE_CAPSULE, owner_id: character, corporation_id: 1, alliance_id: 0, pos: [0.0; 3],
        }).await.ok().unwrap();
        test_world.with(system, move |s| {
            own.install(s, |s, ball| s.add_ship(ball));
            other.install(s, |s, ball| s.add_persistent_ship(ball));
            assert!(s.set_condition_fraction_mask(collateral, 2, 0.5));
            let source = s.prepare_combat_source(ship_id, Some(ship_id), Some(1), None,
                crate::lua::events::resources::FactOrigin::Natural, crate::space::targeting::next_generation(), 1, 1, |_| true).unwrap();
            let damage = crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() };
            assert!(s.apply_damage(ship_id, [1.0, 0.0, 0.0, 0.0], damage, false).unwrap().unwrap().dealt > 0.0);
        }).await.unwrap();
        let mut transition = test_world.with(system, move |s| s.begin_condition_transitions(&[ship_id]).unwrap()).await.unwrap();
        let (token, snapshot) = transition.entries[0].clone();
        let facts = transition.facts.clone().expect("native damage frame");
        assert!(facts.condition_rows().any(|row| row.0 == collateral));
        transition.disarm();
        let (online_tx, _online_rx) = tokio::sync::mpsc::unbounded_channel();
        super::online(&identity, &test_world, &chat, online_tx, None, old_session.clone());
        let (receipt_tx, receipt_rx) = tokio::sync::oneshot::channel();
        let cleanup = Arc::new(super::LogoutCleanup {
            identity, db: test_db.clone(), world: Arc::downgrade(&test_world), chat: Arc::downgrade(&chat),
            sde: sde.clone(), expected: Some(old_session.clone()), space_ship: std::sync::Mutex::new(Some((system, ship_id))), unobserve: std::sync::Mutex::new(None),
            receipt: std::sync::Mutex::new(Some(receipt_tx)),
        });
        let plan = Arc::new(super::LogoutSettlement {
            cleanup, system, ship: ship_id, token, snapshot: snapshot.clone(), facts: Some(facts.clone()),
            position: committed_position, before: std::sync::Mutex::new(None), bounties: None,
            logoff_possible: true, skipped_logoff: std::sync::atomic::AtomicBool::new(false), before_logoff: std::sync::Mutex::new(None),
        });
        let (_, pos, rows) = plan.read_checkpoint(&test_db).await.unwrap();
        *plan.before.lock().unwrap() = Some((pos, rows));
        *plan.before_logoff.lock().unwrap() = Some(characters::selection::logout_timestamps(&test_db, character).await.unwrap());
        test_db.transaction(|tx, outbox: &mut crate::db::outbox::Outbox| {
            let snapshot = snapshot.clone();
            let facts = facts.clone();
            Box::pin(async move {
                super::super::combat::checkpoint_conditions_in(tx, outbox, system, &[snapshot], Some(&facts)).await?;
                characters::selection::logoff(tx, character).await.map_err(crate::services::db_err)?;
                outbox.event(crate::lua::events::GameEvent::CharacterLoggedOff { user_id: account, char_id: character });
                Ok::<_, crate::services::CallError>(())
            })
        }, |(), _| async move { Ok(()) }).await.unwrap();
        let generation = plan.snapshot.generation;
        let callback = plan.callback();
        assert!(test_world.with(system, move |s| s.mark_pending_transition(ship_id, token, generation, callback)).await.unwrap());
        plan.reconcile().await;
        assert!(receipt_rx.await.unwrap().is_ok());
        assert!(test_world.with(system, move |s| {
            let at = s.combat_fact_time();
            s.capture_dirty_condition(collateral, true, at).is_some()
        }).await.unwrap());
        test_world.remove_ball(system, collateral).await;
        test_db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&collateral]).await.unwrap();
        test_db.execute("DELETE FROM game.items WHERE item_id=$1", &[&collateral]).await.unwrap();
    }).await;
    world.remove_ball(system, ship_id).await;
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$1)", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE subject_character_id=$1", &[&character]).await.unwrap();
    world.unregister_pilot_generation(character, None);
    combat::state().active.remove(&ship_id);
    world.unregister_pilot_generation(peer, None);
    combat::reset_pilot(&world, character, Some((system, ship_id)));
    combat::state().active.remove(&peer_ship);
    world.remove_ball(system, peer_ship).await;
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&peer_ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&peer_ship]).await.unwrap();
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$1)", &[&peer]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE subject_character_id=$1", &[&peer]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&peer]).await.unwrap();
    db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship_id]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&ship_id]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship_id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
    result.unwrap();
}
