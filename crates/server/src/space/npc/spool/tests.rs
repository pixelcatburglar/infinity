//! The shared NPC spool: generation-fenced bind, advance and reset semantics.
use super::*;
use crate::services::effects::spool::{SpoolBinding, SpoolParameters};

const PARAMS: SpoolParameters = SpoolParameters { step: 0.07, maximum: 0.14 };

/// A live NPC aiming at a conditioned victim: memberships and the brain's
/// target generation line up exactly like a dispatched `NpcAction::Shot`.
fn engaged() -> (System, SpoolBinding) {
    let (mut s, _, _) = setup(Some(HOSTILE_TO_ALL));
    s.insert(ball(2, Some(10), 50.0));
    install_npc_condition(&mut s, 2);
    let binding = SpoolBinding {
        source_membership: s.combat_membership_generation(1000).unwrap(),
        activation_generation: s.brains[&1000].target_generation,
        target: 2,
        target_membership: s.combat_membership_generation(2).unwrap(),
    };
    let brain = s.brains.get_mut(&1000).unwrap();
    brain.target = Some((2, 10, 0));
    brain.announced = true;
    (s, binding)
}

#[test]
fn first_shot_binds_at_base_and_the_ramp_saturates() {
    let (mut s, binding) = engaged();
    assert_eq!(s.npc_spool_current(1000, binding, PARAMS), Some(0.0));
    assert!(s.npc_spool_advance(1000, binding, PARAMS));
    for _ in 0..4 { assert!(s.npc_spool_advance(1000, binding, PARAMS)); }
    assert_eq!(s.npc_spool_current(1000, binding, PARAMS), Some(PARAMS.maximum));
}

#[test]
fn stale_identity_and_old_worker_never_touch_the_live_ramp() {
    let (mut s, binding) = engaged();
    s.npc_spool_current(1000, binding, PARAMS);
    s.npc_spool_advance(1000, binding, PARAMS);
    let mut stale = binding;
    stale.activation_generation += 1;
    assert_eq!(s.npc_spool_current(1000, stale, PARAMS), None);
    assert!(!s.npc_spool_advance(1000, stale, PARAMS));
    s.npc_spool_reset(1000, stale);
    assert_eq!(s.npc_spool_current(1000, binding, PARAMS), Some(PARAMS.step));
}

#[test]
fn refused_shot_and_target_loss_restart_the_next_shot_at_base() {
    let (mut s, binding) = engaged();
    s.npc_spool_current(1000, binding, PARAMS);
    s.npc_spool_advance(1000, binding, PARAMS);
    s.npc_spool_reset(1000, binding);
    assert_eq!(s.npc_spool_current(1000, binding, PARAMS), Some(0.0));
    s.npc_spool_advance(1000, binding, PARAMS);
    let mut brain = s.brains.remove(&1000).unwrap();
    s.lose_npc_target(1000, &mut brain);
    assert!(brain.turret_spool.binding().is_none() && brain.beam.is_none());
}

#[test]
fn reset_then_accepted_shot_leaves_exactly_one_step() {
    let (mut s, binding) = engaged();
    s.npc_spool_current(1000, binding, PARAMS);
    s.npc_spool_advance(1000, binding, PARAMS);
    // Actor order: a refused shot's reset lands before the next shot's
    // accepted advance, so that shot applies exactly one step and no later
    // reset can strip it.
    s.npc_spool_reset(1000, binding);
    s.npc_spool_current(1000, binding, PARAMS);
    s.npc_spool_advance(1000, binding, PARAMS);
    assert_eq!(s.npc_spool_current(1000, binding, PARAMS), Some(PARAMS.step));
}
