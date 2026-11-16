use super::*;
use std::future::Future;
mod drone;

fn changed(s: &System) -> Vec<Position> {
    s.pilots().filter_map(|ball| {
        let &(generation, previous) = s.snapshot_positions.get(&ball.id)?;
        (ball.pos != previous).then_some(Position { id: ball.id, generation, pos: ball.pos })
    }).collect()
}

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: true, interactive: pilot.is_some(), warp: crate::space::WarpState::default(),
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn candidates_only_include_changed_pilot_ids_and_generations() {
    crate::tests::init_config();
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.insert(ball(2, Some(20), 0.0));
    s.insert(ball(3, None, 0.0));
    assert!(candidates(&s).next().is_none());
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.command(3, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(1000);
    let generation = s.snapshot_positions[&1].0;
    assert_eq!(candidates(&s).collect::<Vec<_>>(), vec![(1, generation)]);
}

#[test]
fn changed_pilots_retry_conflicts_and_acknowledge_only_written_positions() {
    crate::tests::init_config();
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.insert(ball(2, Some(20), 0.0));
    s.insert(ball(3, None, 0.0));
    assert!(changed(&s).is_empty());
    for id in 1..=3 {
        s.command(id, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    }
    s.tick(1000);
    let positions = changed(&s);
    assert_eq!(positions.len(), 2);
    acknowledge(&mut s, positions, &[1].into_iter().collect());
    let remaining = changed(&s);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, 2);
    acknowledge(&mut s, remaining, &[2].into_iter().collect());
    assert!(changed(&s).is_empty());
}

#[test]
fn old_acknowledgement_cannot_mark_reinserted_ship_written() {
    crate::tests::init_config();
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(1000);
    let old = changed(&s);
    s.take(1);
    s.insert(ball(1, Some(10), 0.0));
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(2000);
    acknowledge(&mut s, old, &[1].into_iter().collect());
    assert_eq!(changed(&s).len(), 1);
}

#[test]
fn free_hull_stale_acknowledgement_does_not_clear_replacement() {
    crate::tests::init_config();
    let mut s = System::default();
    s.add_persistent_ship(ball(1, None, 0.0));
    assert_eq!(s.persistent_balls().count(), 1);
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(1000);
    let generation = s.persistent_generation(1).unwrap();
    let old = vec![Position { id: 1, generation, pos: [5.0, 0.0, 0.0] }];
    s.take(1);
    s.add_persistent_ship(ball(1, None, 0.0));
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(2000);
    acknowledge(&mut s, old, &[1].into_iter().collect());
    assert_eq!(candidates(&s).collect::<Vec<_>>(), vec![(1, s.persistent_generation(1).unwrap())]);
}

#[test]
fn no_pilots_keep_cold_skip_and_no_deadline() {
    crate::tests::init_config();
    let mut s = System::default();
    s.insert(ball(1, None, 0.0));
    s.tick(seconds_to_ms(crate::config::game().space.position_snapshot_secs as f64) as i64);
    assert!(s.is_cold(true));
    assert_eq!(s.snapshot_due, 0);
    assert!(changed(&s).is_empty());
}

#[test]
fn snapshot_deadline_follows_config_and_resets_when_pilots_leave() {
    crate::tests::init_config();
    let mut s = System::default();
    let interval = seconds_to_ms(crate::config::game().space.position_snapshot_secs as f64) as i64;
    s.insert(ball(1, Some(10), 0.0));
    assert_eq!(s.snapshot_due, interval);
    s.tick(interval - 1);
    assert_eq!(s.snapshot_due, interval);
    s.tick(interval);
    assert_eq!(s.snapshot_due, interval * 2);
    s.take(1);
    s.tick(interval + 1);
    assert_eq!(s.snapshot_due, 0);
}

#[test]
fn candidates_follow_boundary_motion_not_mailbox_time() {
    crate::tests::init_config();
    let mut s = System::default();
    s.insert(ball(1, Some(10), 0.0));
    s.command(1, |b, _, _| {
        b.goto_direction([1.0, 0.0, 0.0]);
        b.speed_fraction = 1.0;
        Vec::new()
    });
    s.tick(999);
    assert!(candidates(&s).next().is_none());
    s.tick(1000);
    let positions = changed(&s);
    assert_eq!(positions.len(), 1);
    assert!(positions[0].pos[0] > 0.0);
    assert_eq!(positions[0].pos, s.ball(1).unwrap().pos);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn stationary_condition_and_moving_position_keep_independent_acknowledgements() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        if ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) { return false; }
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        stats.max.iter().all(|v| v.is_finite() && *v > 10.0)
            && stats.resonance[0][0].is_finite() && stats.resonance[0][0] > 0.0
    }).unwrap().type_id;
    let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ty, Vec::new());
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    let mut s = System::new(1, Arc::default(), 0);
    s.insert(Ball { type_id: ty, ..ball(1, Some(10), 0.0) });
    s.install_condition(1, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now(),
    }, fit);
    assert!(s.set_condition_fraction_mask(1, 1, 0.9));
    let at = s.combat_fact_time();
    let capture = s.capture_dirty_condition(1, true, at).unwrap();
    assert!(changed(&s).is_empty(), "stationary damage remains a checkpoint candidate");
    s.command(1, |b, _, _| { b.pos[0] = 5.0; Vec::new() });
    s.tick(1000);
    let positions = changed(&s);
    assert_eq!(positions.len(), 1);
    assert!(s.set_condition_fraction_mask(1, 1, 0.8));
    acknowledge(&mut s, positions, &[1].into_iter().collect());
    assert!(!s.acknowledge_condition(1, capture.generation, capture.revision));
    let at = s.combat_fact_time();
    assert!(s.capture_dirty_condition(1, true, at).is_some());
    assert!(changed(&s).is_empty());
    let generation = crate::space::targeting::next_generation();
    let source = s.prepare_combat_source(1, Some(2), Some(3), None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
    let damage = crate::services::combat::damage::DamageSource { facts: Some(source), ..Default::default() };
    assert!(s.apply_damage(1, [1.0, 0.0, 0.0, 0.0], damage, false).unwrap().unwrap().dealt > 0.0);
    s.finish_combat_attempt(&source);
    s.retire_combat_source(generation).unwrap();
    let transition = s.begin_condition_transitions(&[1]).unwrap();
    let frame = transition.facts.as_ref().expect("accepted damage sealed with transition");
    assert!(frame.events().any(|captured| matches!(captured.event, crate::lua::events::GameEvent::DamageApplied { .. })));
    assert!(frame.condition_rows().any(|row| row.0 == 1));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn dying_checkpoint_saves_prelethal_dirty_condition() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        if ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) { return false; }
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        stats.max.iter().all(|v| v.is_finite() && *v > 10.0)
    }).unwrap().type_id;
    let fit = crate::services::fit::Fit::for_space_type(sde, ty, Vec::new());
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    let mut system = System::default();
    system.insert(Ball { type_id: ty, ..ball(1, Some(10), 0.0) });
    system.install_condition(1, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now(),
    }, fit);
    assert!(system.set_condition_fraction_mask(1, 2, 0.75));
    let before = system.combat_condition_snapshot(1).unwrap();
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(1, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
    let damage = crate::services::combat::damage::DamageSource {
        attacker_id: source.actual_actor_character, attacker_ball_id: Some(source.entity.item_id),
        source_ship_id: source.context.ship_id, source_ship_type: source.context.ship_type_id,
        facts: Some(source), force_origin: Some(source.context.origin), ..Default::default()
    };
    assert!(system.apply_damage(1, [0.0; 4], damage, true).unwrap().unwrap().first_lethal.is_some());
    system.finish_combat_attempt(&source);
    system.retire_combat_source(generation).unwrap();
    let at = system.combat_fact_time();
    let captured = system.capture_dirty_condition(1, true, at).unwrap();
    assert_eq!(captured.revision, before.revision);
    assert_eq!(captured.armor_damage, before.condition.stats.max[1] - before.condition.now[1]);
    assert_eq!(captured.hull_damage, before.condition.stats.max[2] - before.condition.now[2]);
    assert!(!system.acknowledge_condition(1, captured.generation, captured.revision));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn periodic_checkpoint_opts_in_but_final_flush_does_not() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let baseline: String = db.query_one("SELECT current_setting('synchronous_commit') AS sc", &[]).await.unwrap().get("sc");
    assert_ne!(baseline, "off");
    let system_id = 910_001_i64;
    let item: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES (1, 1, $1, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&system_id]).await.unwrap().get("item_id");
    let name = format!("snapshot_commit_mode_{item}");
    db.execute(&format!("CREATE FUNCTION game.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.item_id = {item} THEN NEW.custom_info := current_setting('synchronous_commit'); END IF; RETURN NEW; END $$"), &[]).await.unwrap();
    let created = db.execute(&format!("CREATE TRIGGER {name} BEFORE UPDATE OF x, y, z ON game.items FOR EACH ROW WHEN (OLD.item_id = {item}) EXECUTE FUNCTION game.{name}()"), &[]).await;
    let mut body = std::pin::pin!(async {
        created?;
        let started = now_ms();
        let mut system = System::new(system_id, Arc::default(), started);
        system.add_persistent_ship(ball(item, None, 0.0));
        system.snapshot_due = i64::MAX;
        let systems = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let handle = actor::start(system, system_id, Arc::downgrade(&systems), db.clone());
        systems.lock().unwrap().insert(system_id, handle.clone());
        handle.with(move |s| { s.command(item, |b, _, _| { b.pos[0] = 5.0; Vec::new() }); s.tick(s.now_ms() + 1000); schedule(s); }).await.unwrap();
        let _gate = handle.gate.lock().await;
        let row = db.query_one("SELECT x, custom_info FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, f64>("x"), row.get::<_, Option<String>>("custom_info").as_deref()), (5.0, Some("off")));
        drop(_gate);
        handle.with(move |s| { s.snapshot_due = i64::MAX; s.command(item, |b, _, _| { b.pos[0] = 9.0; Vec::new() }); s.tick(s.now_ms() + 1000); }).await.unwrap();
        assert_eq!(flush(handle.clone(), db.clone()).await.unwrap(), 1);
        let row = db.query_one("SELECT x, custom_info FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, f64>("x"), row.get::<_, Option<String>>("custom_info").as_deref()), (9.0, Some(baseline.as_str())));
        assert_eq!(flush(handle, db.clone()).await.unwrap(), 0);
        let row = db.query_one("SELECT x, custom_info FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
        assert_eq!((row.get::<_, f64>("x"), row.get::<_, Option<String>>("custom_info").as_deref()), (9.0, Some(baseline.as_str())));
        Ok::<(), anyhow::Error>(())
    });
    let outcome = std::future::poll_fn(|cx| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body.as_mut().poll(cx))) {
        Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
        Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
        Err(panic) => std::task::Poll::Ready(Err(panic)),
    }).await;
    let _ = db.execute(&format!("DROP TRIGGER IF EXISTS {name} ON game.items"), &[]).await;
    let _ = db.execute(&format!("DROP FUNCTION IF EXISTS game.{name}()"), &[]).await;
    let _ = db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await;
    match outcome { Ok(result) => result.unwrap(), Err(panic) => std::panic::resume_unwind(panic) }
}

fn loaded_system(quantity: i64) -> System {
    crate::tests::init_config();
    let fit = crate::services::fit::Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, vec![]);
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    let mut system = System::default();
    system.install_condition(7, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    let membership = system.combat_membership_generation(7).unwrap();
    assert!(system.publish_loaded_absolute(7, membership, vec![crate::space::combat::loaded::LoadedSeed {
        module: 11, flag: 27, type_id: Some(9), quantity, pending: false }]));
    system
}

#[test]
fn compare_value_clear_keeps_moved_absolute_dirty() {
    let mut system = loaded_system(8);
    let permit = system.hold_loaded_use(7, 11).unwrap();
    assert_eq!(system.consume_loaded(permit), Some((9, 7)));
    let captured = system.capture_dirty_loaded(&[]);
    let permit = system.hold_loaded_use(7, 11).unwrap();
    assert_eq!(system.consume_loaded(permit), Some((9, 6)));
    system.clear_saved_loaded(&captured);
    assert_eq!(system.capture_dirty_loaded(&[]).len(), 1, "a moved value is never cleared");
    let captured = system.capture_dirty_loaded(&[]);
    system.clear_saved_loaded(&captured);
    assert!(system.capture_dirty_loaded(&[]).is_empty());
}

#[test]
fn cancelled_flush_rewrites_the_same_absolute() {
    let mut system = loaded_system(8);
    let permit = system.hold_loaded_use(7, 11).unwrap();
    assert_eq!(system.consume_loaded(permit), Some((9, 7)));
    let first = system.capture_dirty_loaded(&[]);
    let second = system.capture_dirty_loaded(&[]);
    assert_eq!(second.len(), 1);
    assert_eq!((first[0].quantity, second[0].quantity), (7, 7), "unknown results replay absolute, never debit twice");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn loaded_flush_writes_absolute_and_reseeds_zero_match() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let system_id = 910_002_i64;
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z) VALUES (1, 1, $1, 0, 1, true, 0, 0, 0) RETURNING item_id", &[&system_id]).await.unwrap().get("item_id");
    let module: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES (1, 1, $1, 27, 1, true) RETURNING item_id", &[&ship]).await.unwrap().get("item_id");
    db.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1, 27, 9, 5)", &[&ship]).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    world.system(system_id).await.unwrap();
    world.with(system_id, move |s| {
        let fit = crate::services::fit::Fit::for_space_type(sde.clone(), ty, vec![]);
        let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
        s.install_condition(ship, crate::services::combat::groups_health::Condition {
            now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
        let membership = s.combat_membership_generation(ship).unwrap();
        s.publish_loaded_absolute(ship, membership, vec![crate::space::combat::loaded::LoadedSeed {
            module, flag: 27, type_id: Some(9), quantity: 8, pending: false }]);
        let permit = s.hold_loaded_use(ship, module).unwrap();
        s.consume_loaded(permit);
    }).await.unwrap();
    world.flush_positions().await.unwrap();
    assert_eq!(crate::dao::items::charges::loaded(&db, ship, 27).await.unwrap().unwrap().quantity, 7);
    world.flush_positions().await.unwrap();
    assert_eq!(crate::dao::items::charges::loaded(&db, ship, 27).await.unwrap().unwrap().quantity, 7, "clean slots write nothing");
    // 0-match: the durable row's type no longer matches the captured absolute.
    world.with(system_id, move |s| {
        let membership = s.combat_membership_generation(ship).unwrap();
        s.publish_loaded_absolute(ship, membership, vec![crate::space::combat::loaded::LoadedSeed {
            module, flag: 27, type_id: Some(10), quantity: 3, pending: false }]);
        let permit = s.hold_loaded_use(ship, module).unwrap();
        s.consume_loaded(permit);
    }).await.unwrap();
    world.flush_positions().await.unwrap();
    let view = world.with(system_id, move |s| s.loaded_effective(ship, module)).await.flatten();
    let durable = crate::dao::items::charges::loaded(&db, ship, 27).await.unwrap().unwrap();
    assert_eq!((durable.type_id, durable.quantity), (9, 7), "0-match flush leaves the durable row untouched");
    let effective = view.map(|v| (v.1, v.2));
    assert_ne!(effective, Some((10, 2)), "stale captured absolute must not survive reseed");
    assert_eq!(effective, Some((durable.type_id, durable.quantity)), "unmatched rows reseed to durable truth");
    db.execute("DELETE FROM game.module_charges WHERE ship_id = $1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&&[ship, module][..]]).await.unwrap();
}

fn bounty_call_err(e: crate::services::CallError) -> anyhow::Error {
    match e {
        crate::services::CallError::Internal(e) => e,
        crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn bounty_only_snapshot_is_durable_and_stale_capture_cannot_resurrect() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let system_id = 910_002_i64;
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let account = crate::dao::accounts::authentication::register(&db, &format!("bounty-snap-{seq}"), b"f", 0, 0).await.unwrap().user_id;
    let owner: i64 = db.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,1,1,1,1) RETURNING character_id", &[&account, &format!("bounty-snap-{seq}")]).await.unwrap().get("character_id");
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    world.system(system_id).await.unwrap();
    // A bounty-only dirty actor still reaches the snapshot writer.
    world.with(system_id, move |s| s.add_npc_bounty(owner, 42, crate::isk::IskAmount::from_cents(1234).unwrap(), crate::client::filetime_now())).await.unwrap().unwrap();
    world.flush_positions().await.unwrap();
    let rows = crate::dao::npc_bounties::for_character(&db, owner).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].system_id, rows[0].kill_count, rows[0].amount), (system_id, 1, crate::isk::IskAmount::from_cents(1234).unwrap()));
    let job_id = format!("npc-bounty:{}:{}", owner, rows[0].batch.started_at);
    assert_eq!(db.query_one("SELECT kind FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap().get::<_, String>("kind"), "npc.bounty.payout");
    // The same writer covers the next kill; nothing accumulates.
    world.with(system_id, move |s| s.add_npc_bounty(owner, 42, crate::isk::IskAmount::from_cents(5).unwrap(), crate::client::filetime_now())).await.unwrap().unwrap();
    world.flush_positions().await.unwrap();
    assert_eq!(crate::dao::npc_bounties::for_character(&db, owner).await.unwrap()[0].kill_count, 2);
    // A stale writer kept from before the payout must be refused afterward and
    // must never resurrect a paid row.
    let stale = world.with(system_id, move |s| s.capture_npc_bounties_final(owner)).await.unwrap().unwrap().unwrap();
    crate::services::bounty::npc::settle_due(&db, &world, (owner, rows[0].batch.started_at), i64::MAX / 2).await.unwrap();
    assert!(crate::dao::npc_bounties::for_character(&db, owner).await.unwrap().is_empty());
    let refused = db.transaction(|tx, _: &mut ()| {
        let stale = stale.clone();
        Box::pin(async move { stale.write_in(tx).await.map_err(|e| crate::db::Error::Progress(bounty_call_err(e))) })
    }, |value, ()| async move { Ok(value) }).await;
    assert!(refused.is_err());
    assert!(crate::dao::npc_bounties::for_character(&db, owner).await.unwrap().is_empty(), "stale writer resurrected a paid row");
    assert_eq!(crate::services::bounty::npc::resolve_receipt(&db, &stale).await.unwrap(), crate::services::bounty::npc::checkpoint::CheckpointOutcome::Unchanged);
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap();
    db.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=ANY($1))", &[&&[owner][..]]).await.unwrap();
    db.execute("DELETE FROM game.wallet_accounts WHERE owner_id=ANY($1)", &[&&[owner][..]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
