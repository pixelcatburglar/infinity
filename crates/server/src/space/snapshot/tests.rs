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
        warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
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
