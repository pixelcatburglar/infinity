use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
mod drone;

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: pilot.map(|_| 1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: pilot.is_some(),
        massive: true, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0,
        slim: Vec::new(),
    }
}
fn setup(owner: Option<i64>) -> (System, mpsc::UnboundedReceiver<EffectJob>, mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    crate::tests::init_config();
    let registry = Arc::new(Pilots::default());
    let (notify, notifications) = mpsc::unbounded_channel();
    registry.register_pilot(10, notify);
    let mut s = System::new(1, registry, 0);
    let (tx, rx) = mpsc::unbounded_channel();
    s.effect_jobs = Some(tx);
    s.insert(ball(1000, None, 0.0));
    s.add_npc(1000, NpcBrain::new(owner, NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0, scan_resolution: 17_163.0,
        repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_| panic!("effects must run outside the actor"))));
    (s, rx, notifications)
}
fn jam_setup(id: i64) -> (System, mpsc::UnboundedReceiver<EffectJob>, mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    let (mut s, jobs, notifications) = setup(Some(HOSTILE_TO_ALL));
    let brain = s.brains.remove(&1000).unwrap();
    s.remove_ball(1000);
    s.insert(ball(id, None, 0.0));
    assert!(s.add_npc(id, brain));
    s.insert(ball(1, Some(10), 50.0));
    s.brains.get_mut(&id).unwrap().next_think_ms = 0;
    (s, jobs, notifications)
}
fn add_assist_npc(s: &mut System, id: i64, owner: i64, x: f64) {
    let mut b = ball(id, None, x);
    b.owner_id = owner;
    s.insert(b);
    s.add_npc(id, NpcBrain::new(None, NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_| panic!("effects must run outside the actor"))));
}

#[test]
fn assistance_selects_lowest_fraction_then_id_and_respects_groups_and_range() {
    let (mut s, _, _) = setup(None);
    add_assist_npc(&mut s, 2, 1, 10.0);
    add_assist_npc(&mut s, 3, 1, 10.0);
    add_assist_npc(&mut s, 4, 1, 100.0);
    add_assist_npc(&mut s, 5, 2, 10.0);
    s.insert(ball(1, Some(10), 0.0));
    let select = |s: &System| s.assist_target(1000, |_, target| {
        if target.id == 4 { return None; }
        let fraction = match target.id { 2 | 3 => 0.2, _ => 0.1 };
        (fraction < 1.0).then_some(fraction)
    }).map(|ball| ball.id);
    assert_eq!(select(&s), Some(2));
    s.spawn_group([1000, 3, 4], |_| {});
    assert_eq!(select(&s), Some(3));
    s.remove_ball(3);
    assert_eq!(select(&s), None);
    assert!(s.assist_target(1000, |_, _| None).is_none());
}

#[test]
fn assistance_guard_rejects_hostile_confusion_and_stale_generation() {
    use crate::services::combat::groups_health::NpcCapacitorGuard;
    let (mut s, _, _) = setup(Some(HOSTILE_TO_ALL));
    add_assist_npc(&mut s, 2, 1, 10.0);
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = lock_time_ms(17_163.0, 1.0) as i64;
    s.tick(deadline);
    let generation = s.brains[&1000].target_generation;
    let guard = NpcCapacitorGuard::Assistance { source: 1000, hostile: 1, target: 2, generation, optimal: 20.0, falloff: 0.0 };
    assert!(guard.valid(&s));
    assert!(!NpcCapacitorGuard::Attack {
        source: 1000, target: 2,
        source_guard: Some(crate::services::effects::space_source::SourceGuard::Npc { source: 1000, target: 2, generation }),
        player: None, optimal: 100.0, falloff: 0.0, offensive: true,
    }.valid(&s));
    assert!(!NpcCapacitorGuard::Assistance { source: 1000, hostile: 1, target: 1, generation, optimal: 100.0, falloff: 0.0 }.valid(&s));
    let mut brain = s.brains.remove(&1000).unwrap();
    s.lose_npc_target(1000, &mut brain);
    s.brains.insert(1000, brain);
    assert!(!guard.valid(&s));
}

#[test]
fn npc_acquires_only_on_configured_think_interval() {
    let (mut s, _, _) = setup(Some(10));
    let interval = crate::config::game().combat.npc_think_ms;
    s.tick(0);
    s.insert(ball(1, Some(10), 50.0));
    s.tick(interval - 1);
    assert!(s.brains[&1000].target.is_none());
    s.tick(interval);
    assert_eq!(s.brains[&1000].target.unwrap().0, 1);
}
#[test]
fn nearest_hostile_excludes_cloak_warp_and_outside_aggro() {
    let (mut s, _, _) = setup(Some(10));
    s.insert(ball(1, Some(11), 1.0)); // neutral
    let mut cloaked = ball(2, Some(10), 2.0);
    cloaked.cloak = 1;
    s.insert(cloaked);
    let mut warping = ball(3, Some(12), 3.0);
    warping.warp_to([1_000_000.0, 0.0, 0.0], 0.0, 3000);
    s.insert(warping);
    s.insert(ball(4, Some(13), 101.0));
    s.add_aggressor(1000, 12);
    s.add_aggressor(1000, 13);
    s.tick(0);
    assert!(s.brains[&1000].target.is_none());
    s.insert(ball(5, Some(14), 40.0));
    s.insert(ball(6, Some(15), 60.0));
    s.add_aggressor(1000, 14);
    s.add_aggressor(1000, 15);
    s.tick(1000);
    assert_eq!(s.brains[&1000].target.unwrap().0, 5);
    assert_eq!(s.ball(1000).unwrap().mode, MODE_STOP);
    assert_eq!(s.ball(1000).unwrap().speed_fraction, 0.0);
    s.tick(2000);
    assert_eq!(s.ball(1000).unwrap().mode, MODE_ORBIT);
    assert_eq!(s.ball(1000).unwrap().speed_fraction, 1.0);
}
#[test]
fn owner_and_hostile_to_all_and_aggro_boundary() {
    for owner in [Some(10), Some(HOSTILE_TO_ALL)] {
        let (mut s, _, _) = setup(owner);
        s.insert(ball(1, Some(10), 100.0));
        s.tick(0);
        assert_eq!(s.brains[&1000].target.unwrap().0, 1);
    }
}
#[test]
fn target_loss_stops_and_notifies_beyond_double_aggro() {
    let (mut s, _, mut notify) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    let (tx, mut events) = mpsc::unbounded_channel();
    s.observe(1, 1, tx);
    s.tick(0);
    s.command(1, |b, _, _| { b.pos = [201.0, 0.0, 0.0]; vec![] });
    s.tick(1000);
    events.try_recv().unwrap();
    assert!(s.brains[&1000].target.is_none());
    s.tick(2000);
    assert_eq!(s.ball(1000).unwrap().mode, MODE_STOP);
    assert_eq!(s.ball(1000).unwrap().speed_fraction, 1.0);
    let batch = events.try_recv().unwrap();
    assert!(batch.iter().any(|v| {
        let Value::Tuple(parts) = v else { return false };
        let Value::Tuple(call) = &parts[1] else { return false };
        call[0].as_bytes() == Some(b"Stop")
    }));
    let (name, args) = notify.try_recv().unwrap();
    assert_eq!(name, "OnTarget");
    assert_eq!(args[0].as_bytes(), Some(b"otherlost".as_slice()));
}
#[test]
fn lock_delay_and_shot_throttling() {
    let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = lock_time_ms(17_163.0, 1.0) as i64;
    s.tick(deadline - 1);
    assert!(jobs.try_recv().is_err());
    s.tick(deadline);
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
    assert!(s.brains[&1000].announced);
    s.tick(deadline + 2000);
    assert!(jobs.try_recv().is_err());
    s.tick(deadline + 3000);
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
    assert!(jobs.try_recv().is_err());
}
#[test]
fn targeting_refresh_preserves_pending_deadline_and_rejects_out_of_range() {
    let (mut s, _, _) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = s.brains[&1000].target.unwrap().2;
    s.refresh_npc_targeting(1000, 100.0, Some(60.0));
    assert_eq!(s.brains[&1000].target.unwrap().2, deadline);
    s.refresh_npc_targeting(1000, 100.0, Some(40.0));
    assert!(s.brains[&1000].target.is_none());
    s.tick(1000);
    assert!(s.brains[&1000].target.is_none());
    s.refresh_npc_targeting(1000, 17_163.0, None);
    s.tick(2000);
    assert_eq!(s.brains[&1000].target.unwrap().0, 1);
    assert_eq!(s.brains[&1000].target.unwrap().2, 2000 + lock_time_ms(17_163.0, 1.0) as i64);
}


#[test]
fn projected_cycles_are_independent_and_stale_jobs_cannot_restore_effects() {
    use crate::services::effects::{EffectKind, JammingType, projection::Projection};
    let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
    let brain = s.brains.get_mut(&1000).unwrap();
    brain.add_effect(1, 1000.0);
    brain.add_effect(2, 2000.0);
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = s.brains[&1000].target.unwrap().2;
    s.tick(deadline);
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
    let EffectJob::Npc { action: NpcAction::Effect { source_module: first, generation, .. }, .. } = jobs.try_recv().unwrap() else { panic!("first projection") };
    let EffectJob::Npc { action: NpcAction::Effect { source_module: second, .. }, .. } = jobs.try_recv().unwrap() else { panic!("second projection") };
    assert!(first < 0 && second < 0 && first != second);
    assert!(s.npc_projection_valid(1000, 1, generation));
    s.upsert_projection(1, Projection { source_ship: 1000, source_module: first,
        source_type: 1, effect_id: 1, kind: EffectKind::Projected { jamming: JammingType::Webify },
        until_ms: i64::MAX, values: vec![], charge_values: vec![], charge_type: None });
    s.tick(deadline + 1000);
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Effect { source_module, .. }, .. } if source_module == first));
    assert!(jobs.try_recv().is_err());
    let mut brain = s.brains.remove(&1000).unwrap();
    s.lose_npc_target(1000, &mut brain);
    s.brains.insert(1000, brain);
    assert!(!s.npc_projection_valid(1000, 1, generation));
    assert!(s.projections_on(1).is_empty());
    s.tick(deadline + 2000);
    assert!(!s.npc_projection_valid(1000, 1, generation));
}

#[test]
fn departing_npc_removes_its_projections_and_invalidates_jobs() {
    use crate::services::effects::{EffectKind, JammingType, projection::Projection};
    let (mut s, _, _) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = s.brains[&1000].target.unwrap().2;
    s.tick(deadline);
    let generation = s.brains[&1000].target_generation;
    s.upsert_projection(1, Projection { source_ship: 1000, source_module: -1,
        source_type: 1, effect_id: 1, kind: EffectKind::Projected { jamming: JammingType::Webify },
        until_ms: i64::MAX, values: vec![], charge_values: vec![], charge_type: None });
    s.remove_ball(1000);
    assert!(s.projections_on(1).is_empty());
    assert!(!s.npc_projection_valid(1000, 1, generation));
}

#[test]
fn replacement_rejects_old_lock_and_queued_actions_with_same_target_and_deadline() {
    let (mut s, mut jobs, mut notifications) = setup(Some(HOSTILE_TO_ALL));
    s.brains.get_mut(&1000).unwrap().add_effect(42, 1000.0);
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let (ship, pilot, deadline) = s.brains[&1000].target.unwrap();
    let old_lock = s.brains[&1000].target_generation;
    s.complete_npc_lock(1000, ship, pilot, deadline, old_lock);
    let EffectJob::Npc { action: NpcAction::Shot { generation: old_shot, .. }, .. } = jobs.try_recv().unwrap() else { panic!("expected shot") };
    let EffectJob::Npc { action: NpcAction::Effect { generation: old_effect, .. }, .. } = jobs.try_recv().unwrap() else { panic!("expected effect") };
    assert!(s.npc_projection_valid(1000, ship, old_shot));
    assert!(s.npc_projection_valid(1000, ship, old_effect));
    assert!(notifications.try_recv().is_ok());

    s.remove_ball(1000);
    s.insert(ball(1000, None, 0.0));
    let mut replacement = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_| panic!("effects must run outside the actor")));
    replacement.add_effect(42, 1000.0);
    assert!(s.add_npc(1000, replacement));
    s.brains.get_mut(&1000).unwrap().next_think_ms = s.now_ms();
    s.think_npcs();
    assert_eq!(s.brains[&1000].target, Some((ship, pilot, deadline)));
    let new_lock = s.brains[&1000].target_generation;
    s.complete_npc_lock(1000, ship, pilot, deadline, old_lock);
    assert!(!s.brains[&1000].announced);
    assert!(jobs.try_recv().is_err());
    assert!(notifications.try_recv().is_err());
    s.complete_npc_lock(1000, ship, pilot, deadline, new_lock);
    assert!(s.brains[&1000].announced);
    let EffectJob::Npc { action: NpcAction::Shot { generation: new_shot, .. }, .. } = jobs.try_recv().unwrap() else { panic!("expected replacement shot") };
    let EffectJob::Npc { action: NpcAction::Effect { generation: new_effect, .. }, .. } = jobs.try_recv().unwrap() else { panic!("expected replacement effect") };
    assert!(!s.npc_projection_valid(1000, ship, old_shot));
    assert!(!s.npc_projection_valid(1000, ship, old_effect));
    assert!(s.npc_projection_valid(1000, ship, new_shot));
    assert!(s.npc_projection_valid(1000, ship, new_effect));
}

#[test]
fn effect_timers_stop_on_target_generation_change() {
    let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
    s.brains.get_mut(&1000).unwrap().add_effect(42, 1000.0);
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = lock_time_ms(17_163.0, 1.0) as i64;
    s.tick(deadline);
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
    let generation = match jobs.try_recv().unwrap() {
        EffectJob::Npc { action: NpcAction::Effect { generation, .. }, .. } => generation,
        _ => panic!("expected NPC effect"),
    };
    assert!(s.npc_projection_valid(1000, 1, generation));
    s.tick(deadline + 1000);
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Effect { generation: g, .. }, .. } if g == generation));
    let mut brain = s.brains.remove(&1000).unwrap();
    s.lose_npc_target(1000, &mut brain);
    s.brains.insert(1000, brain);
    assert!(!s.npc_projection_valid(1000, 1, generation));
    s.tick(deadline + 2000);
    assert!(jobs.try_recv().is_err());
}

#[test]
fn effect_registration_rejects_invalid_cycle_lengths() {
    let (mut s, _, _) = setup(None);
    let brain = s.brains.get_mut(&1000).unwrap();
    for duration in [0.0, -2.0, f64::NAN, f64::INFINITY] { brain.add_effect(42, duration); }
    assert!(brain.modules.is_empty());
    brain.add_effect(42, -1.0);
    brain.add_effect(42, 1.0);
    assert_eq!(brain.modules.len(), 2);
    assert_ne!(brain.modules[0].source_module, brain.modules[1].source_module);
}

#[test]
fn stale_and_removed_targets_do_not_complete_locks() {
    for remove_npc in [false, true] {
        let (mut s, mut jobs, mut notifications) = setup(Some(HOSTILE_TO_ALL));
        s.insert(ball(1, Some(10), 50.0));
        s.tick(0);
        let deadline = s.brains[&1000].target.unwrap().2;
        if remove_npc { s.remove_ball(1000); } else { s.remove_ball(1); }
        s.tick(deadline);
        assert!(jobs.try_recv().is_err());
        while let Ok((_, args)) = notifications.try_recv() {
            assert_ne!(args[0].as_bytes(), Some(b"otheradd".as_slice()));
        }
    }
    let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let deadline = s.brains[&1000].target.unwrap().2;
    s.brains.get_mut(&1000).unwrap().target = Some((1, 10, deadline + 100));
    s.tick(deadline);
    assert!(jobs.try_recv().is_err());
}

#[test]
fn failed_lock_reacquires_and_ignores_stale_completion() {
    let (mut s, mut jobs, mut notifications) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(1, Some(10), 50.0));
    s.tick(0);
    let old_deadline = s.brains[&1000].target.unwrap().2;
    let old_generation = s.brains[&1000].target_generation;
    s.command(1, |b, _, _| { b.cloak = 1; vec![] });
    s.tick(1000);
    s.brains.get_mut(&1000).unwrap().next_think_ms = old_deadline + 1;
    s.tick(old_deadline);
    assert!(s.brains[&1000].target.is_none());
    assert!(!s.brains[&1000].announced);
    assert_eq!(s.ball(1000).unwrap().mode, MODE_STOP);
    assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otherlost".as_slice()));
    assert!(jobs.try_recv().is_err());

    s.command(1, |b, _, _| { b.cloak = 0; vec![] });
    s.tick((old_deadline / 1000 + 1) * 1000);
    let new_target = s.brains[&1000].target.unwrap();
    assert!(new_target.2 > old_deadline);
    s.complete_npc_lock(1000, 1, 10, old_deadline, old_generation);
    assert_eq!(s.brains[&1000].target, Some(new_target));
    assert!(!s.brains[&1000].announced);
    assert!(jobs.try_recv().is_err());
    s.tick(new_target.2);
    assert!(s.brains[&1000].announced);
    assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otheradd".as_slice()));
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Shot { .. }, .. }));
}

#[test]
fn jam_cancels_pending_lock_and_prevents_reacquisition_until_expiry() {
    let id = 8_901_001;
    let (mut s, mut jobs, mut notifications) = jam_setup(id);
    s.tick(0);
    let deadline = s.brains[&id].target.unwrap().2;
    crate::services::combat::jamming::apply(&mut s, 1, -8_901_001, id, (deadline + 4000) as f64);
    assert!(s.brains[&id].target.is_none());
    assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otherlost".as_slice()));
    s.brains.get_mut(&id).unwrap().config.repairs = (1.0, 0.0, 5000.0);
    s.brains.get_mut(&id).unwrap().next_think_ms = 0;
    s.think_npcs();
    assert!(matches!(jobs.try_recv().unwrap(), EffectJob::Npc { action: NpcAction::Repair, .. }));
    s.brains.get_mut(&id).unwrap().config.repairs = (0.0, 0.0, 5000.0);
    s.brains.get_mut(&id).unwrap().target = Some((1, 10, deadline));
    let generation = s.brains[&id].target_generation;
    s.complete_npc_lock(id, 1, 10, deadline, generation);
    assert!(s.brains[&id].target.is_none());
    s.tick(deadline);
    assert!(s.brains[&id].target.is_none());
    assert!(jobs.try_recv().is_err());
    s.tick(deadline + 2000);
    assert!(s.brains[&id].target.is_none());
    s.tick(deadline + 4000);
    let now = s.now_ms();
    s.brains.get_mut(&id).unwrap().next_think_ms = now;
    s.think_npcs();
    assert!(s.brains[&id].target.is_some());
}

#[test]
fn jam_drops_completed_lock_and_invalidates_queued_projection() {
    use crate::services::effects::{EffectKind, JammingType, projection::Projection};
    let id = 8_901_002;
    let (mut s, mut jobs, mut notifications) = jam_setup(id);
    s.brains.get_mut(&id).unwrap().add_effect(1, 1000.0);
    s.tick(0);
    let deadline = s.brains[&id].target.unwrap().2;
    s.tick(deadline);
    let EffectJob::Npc { action: NpcAction::Shot { generation: shot_generation, .. }, .. } = jobs.try_recv().unwrap() else { panic!("expected shot") };
    let EffectJob::Npc { action: NpcAction::Effect { source_module, generation, .. }, .. } = jobs.try_recv().unwrap() else { panic!("expected projection") };
    assert!(s.npc_projection_valid(id, 1, shot_generation));
    s.upsert_projection(1, Projection { source_ship: id, source_module, source_type: 1,
        effect_id: 1, kind: EffectKind::Projected { jamming: JammingType::Webify },
        until_ms: i64::MAX, values: vec![], charge_values: vec![], charge_type: None });
    assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otheradd".as_slice()));
    crate::services::combat::jamming::apply(&mut s, 1, -8_901_002, id, 4000.0);
    assert!(s.brains[&id].target.is_none());
    assert!(!s.npc_projection_valid(id, 1, shot_generation));
    assert!(!s.npc_projection_valid(id, 1, generation));
    assert!(s.projections_on(1).is_empty());
    assert_eq!(notifications.try_recv().unwrap().1[0].as_bytes(), Some(b"otherlost".as_slice()));
    s.tick(deadline + 4000);
    let now = s.now_ms();
    s.brains.get_mut(&id).unwrap().next_think_ms = now;
    s.think_npcs();
    let new_deadline = s.brains[&id].target.unwrap().2;
    s.tick(new_deadline);
    assert!(s.brains[&id].announced);
    assert!(!s.npc_projection_valid(id, 1, shot_generation));
    assert!(!s.npc_projection_valid(id, 1, generation));
}

#[test]
fn sampled_acquisition_nearest_loss_and_shot_capture() {
    let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
    let mut moving = ball(1, Some(10), 50.0);
    moving.vel = [1000.0, 0.0, 0.0];
    s.insert(moving);
    let mut other = ball(2, Some(11), 60.0);
    other.pos[1] = 10.0;
    s.insert(other);
    s.tick(1000);
    assert!(s.ball(1).unwrap().pos[0] > 200.0);
    assert_eq!(s.sample_position(1).unwrap()[0], 50.0);
    let (ship, pilot, deadline) = s.brains[&1000].target.unwrap();
    assert_eq!(ship, 1);

    let generation = s.brains[&1000].target_generation;
    s.complete_npc_lock(1000, ship, pilot, deadline, generation);
    assert!(s.brains[&1000].announced);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 1500;
    s.tick(1500);
    assert!(s.sample_position(1).unwrap()[0] > 200.0);
    assert_eq!(s.brains[&1000].target.unwrap().0, 2);
    let EffectJob::Npc { action: NpcAction::Shot { me, victim, .. }, .. } = jobs.try_recv().unwrap() else {
        panic!("expected captured shot");
    };
    assert_eq!(me.pos, [0.0, 0.0, 0.0]);
    assert_eq!(victim.pos, [50.0, 0.0, 0.0]);
    assert_ne!(victim.pos, s.sample_position(1).unwrap());
}

#[test]
fn lock_rejects_sampled_out_of_range_even_when_endpoint_is_inside() {
    let (mut s, mut jobs, _) = setup(Some(HOSTILE_TO_ALL));
    let mut moving = ball(1, Some(10), 250.0);
    moving.vel = [-200.0, 0.0, 0.0];
    s.insert(moving);
    s.brains.get_mut(&1000).unwrap().next_think_ms = 2000;
    s.tick(1000);
    assert!(s.ball(1).unwrap().pos[0] < 200.0);
    assert_eq!(s.sample_position(1).unwrap()[0], 250.0);
    s.brains.get_mut(&1000).unwrap().target = Some((1, 10, 1000));
    let generation = s.brains[&1000].target_generation;
    s.complete_npc_lock(1000, 1, 10, 1000, generation);
    assert!(s.brains[&1000].target.is_none());
    assert!(!s.brains[&1000].announced);
    assert!(jobs.try_recv().is_err());
}

#[tokio::test]
async fn deferred_jobs_progress_while_npc_effects_remain_fifo() {
    let mut s = System::default();
    start_worker(&mut s);
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(Mutex::new(Some(release_rx)));
    let effects: NpcEffects = Arc::new(move |_| {
        let started_tx = started_tx.clone();
        let release = release.lock().unwrap().take();
        Box::pin(async move {
            started_tx.send(()).unwrap();
            if let Some(release) = release { let _ = release.await; }
        })
    });
    let tx = s.effect_jobs.as_ref().unwrap();
    assert!(tx.send(EffectJob::Npc { effects: effects.clone(), action: NpcAction::Repair }).is_ok());
    assert!(tx.send(EffectJob::Npc { effects, action: NpcAction::Repair }).is_ok());
    started_rx.recv().await.unwrap();
    let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    s.defer(async move { let _ = entered_tx.send(()); let _ = blocked_rx.await; });
    entered_rx.await.unwrap();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    s.defer(async move { let _ = done_tx.send(()); });
    done_rx.await.unwrap();
    blocked_tx.send(()).unwrap();
    assert!(started_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    started_rx.recv().await.unwrap();
}

#[tokio::test]
async fn timer_defers_async_work_without_polling_inside_actor() {
    let (mut s, _jobs, _) = setup(None);
    let (tx, mut deferred_jobs) = mpsc::unbounded_channel();
    s.deferred_jobs = Some(tx);
    let count = Arc::new(AtomicUsize::new(0));
    let deferred = count.clone();
    s.schedule(100, move |s| {
        s.defer(async move { deferred.fetch_add(1, Ordering::Relaxed); });
    });
    s.tick(100);
    assert_eq!(count.load(Ordering::Relaxed), 0);
    let future = deferred_jobs.try_recv().unwrap();
    future.await;
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert!(deferred_jobs.try_recv().is_err());
}
#[test]
fn groups_clear_once_for_destroyed_removed_and_missing_members() {
    let (mut s, _, _) = setup(None);
    assert!(!s.is_cold(true));
    s.insert(ball(2, None, 0.0));
    let cleared = Arc::new(AtomicUsize::new(0));
    let count = cleared.clone();
    let group = s.spawn_group([1000, 2, 999], move |s| {
        assert!(!s.contains(1000) && !s.contains(2));
        count.fetch_add(1, Ordering::Relaxed);
    });
    assert_eq!(s.group_remaining(group), 2);
    s.destroy_ball(1000);
    assert!(!s.brains.contains_key(&1000));
    assert_eq!(s.group_remaining(group), 1);
    assert_eq!(cleared.load(Ordering::Relaxed), 0);
    s.remove_ball(2);
    s.remove_ball(2);
    s.destroy_ball(1000);
    assert_eq!(s.group_remaining(group), 0);
    assert_eq!(cleared.load(Ordering::Relaxed), 1);
    let count = cleared.clone();
    let empty = s.spawn_group([], move |_| { count.fetch_add(1, Ordering::Relaxed); });
    assert_eq!(s.group_remaining(empty), 0);
    assert_eq!(cleared.load(Ordering::Relaxed), 2);
}
#[test]
fn group_handles_are_unique_across_system_reloads() {
    let (mut before, _, _) = setup(None);
    let old = before.spawn_group([1000], |_| {});
    let (mut after, _, _) = setup(None);
    let new = after.spawn_group([1000], |_| {});
    assert_ne!(old, new);
    assert_eq!(after.group_remaining(old), 0);
    assert_eq!(after.group_remaining(new), 1);
}
