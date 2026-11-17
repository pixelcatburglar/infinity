//! NPC turret shot capture: the resident-fit packet, the 6995 binding and the
//! all-zero damage reset. Ramp arithmetic lives with the brain.
use super::*;
use crate::services::effects::spool::SpoolParameters;
use infinity_core::consts::dogma::EFFECT_TARGET_DISINTEGRATOR_ATTACK;

fn stats(turret_effect: Option<i32>) -> NpcStats {
    NpcStats { repairs: (0.0, 0.0, 1000.0), scan_resolution: 1.0, fly_range: 1.0, aggro_range: 1e9,
        rate_of_fire_ms: 1e9, optimal: 5_000.0, falloff: 1_000.0, tracking: 1.0, opt_sig: 1.0,
        damage: [1.0, 0.0, 0.0, 0.0], missile: None, fx: None, turret_effect }
}

/// A conditioned NPC aiming at a conditioned victim, exactly like a dispatched
/// `NpcAction::Shot`: memberships and the brain's target generation line up.
fn engaged() -> (System, i64, i64, u64) {
    crate::tests::init_config();
    let mut s = System::new(1, Arc::new(crate::space::Pilots::default()), 0);
    let mut me = remote_repair_db::ball(1000, 1, 1);
    me.char_id = None;
    me.signature_radius = Some(1.0);
    s.insert(me);
    let mut victim = remote_repair_db::ball(2000, 1, 1);
    victim.pos = [1.0, 0.0, 0.0];
    victim.signature_radius = Some(1.0);
    s.insert(victim);
    let fit = Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, Vec::new());
    let ship_stats = ShipStats::from_fit(&fit, Key::Ship);
    let second = Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, Vec::new());
    for (id, fit) in [(1000, fit), (2000, second)] {
        s.install_condition(id, Condition { now: ship_stats.max,
            stats: ship_stats.clone(), shield_at: filetime_now() }, fit);
    }
    let mut brain = NpcBrain::new(None, NpcConfig { fly_range: 1.0, aggro_range: 1e9,
        rate_of_fire_ms: 1e9, scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0) }, Arc::new(|_, _| None));
    let generation = brain.target_generation();
    brain.target = Some((2000, 1, 0));
    brain.announced = true;
    assert!(s.add_npc(1000, brain));
    (s, 1000, 2000, generation)
}

#[test]
fn capture_binds_6995_and_ordinary_capture_carries_none() {
    let (mut s, id, target, generation) = engaged();
    let (binding, _) = capture_shot(&mut s, id, target, generation,
        &stats(Some(EFFECT_TARGET_DISINTEGRATOR_ATTACK)))
        .and_then(|inputs| inputs.spool).expect("6995 capture carries a spool");
    assert_eq!((binding.target, binding.activation_generation,
        binding.source_membership, binding.target_membership),
        (target, generation, s.combat_membership_generation(id).unwrap(),
            s.combat_membership_generation(target).unwrap()));
    let ordinary = capture_shot(&mut s, id, target, generation, &stats(None)).expect("ordinary capture");
    assert!(ordinary.spool.is_none() && ordinary.damage == [1.0, 0.0, 0.0, 0.0]);
}

#[test]
fn zero_damage_6995_capture_resets_the_bound_ramp() {
    let (mut s, id, target, generation) = engaged();
    let spool_stats = stats(Some(EFFECT_TARGET_DISINTEGRATOR_ATTACK));
    let (binding, _) = capture_shot(&mut s, id, target, generation, &spool_stats)
        .and_then(|inputs| inputs.spool).expect("6995 capture carries a spool");
    let parameters = SpoolParameters { step: 0.07, maximum: 0.14 };
    s.npc_spool_current(id, binding, parameters);
    assert!(s.npc_spool_advance(id, binding, parameters));
    // The fixture's resident fit deals no turret damage, so the next capture
    // of the same activation ends the bound ramp inside the same actor turn.
    let inputs = capture_shot(&mut s, id, target, generation, &spool_stats).expect("capture");
    assert!(inputs.damage.iter().all(|damage| *damage <= 0.0));
    assert_eq!(s.npc_spool_current(id, binding, parameters), Some(0.0));
}
