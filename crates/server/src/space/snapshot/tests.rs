use super::*;
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
