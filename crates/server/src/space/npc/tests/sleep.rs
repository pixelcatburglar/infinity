//! In-process NPC sleep: ownership move, readiness refusal and ledger identity.
//! The belt's own SDE proof needs a real map row, so the clean fixture cohort
//! always stops at `npc_sleep_origin`; earlier refusals and the structural
//! move/install are still exercised exactly.
use super::*;
use crate::space::combat_facts::{CombatFacts, CombatSource, Refusal};
use crate::space::system::npc_sleep::NpcSpawnOrigin;

fn brain(owner: Option<i64>) -> NpcBrain {
    NpcBrain::new(owner, NpcConfig {
        fly_range: 20.0, aggro_range: 100.0, rate_of_fire_ms: 2500.0,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 5000.0),
    }, Arc::new(|_, _| None))
}
fn cohort_with(edit: impl FnOnce(&mut Ball)) -> System {
    crate::tests::init_config();
    let mut s = System::new(7, Arc::new(Pilots::default()), 0);
    s.projection_sde = Some(crate::sde::types::tests::combat_fixture());
    let mut b = ball(1000, None, 0.0);
    edit(&mut b);
    s.insert(b);
    install_npc_condition(&mut s, 1000);
    assert!(s.add_npc(1000, brain(Some(HOSTILE_TO_ALL))));
    let group = s.spawn_group(vec![1000], |_| {});
    assert!(s.mark_npc_group_sleepable(group, NpcSpawnOrigin::Belt(77)));
    s
}
fn cohort() -> System { cohort_with(|_| {}) }
fn blocked_with(edit: impl FnOnce(&mut System)) -> Option<&'static str> { let mut s = cohort(); edit(&mut s); s.npc_sleep_blocked() }

#[test]
fn sleep_move_and_restore_preserves_survivor_exact_state() {
    let (mut s, _jobs, _notify) = setup(Some(HOSTILE_TO_ALL));
    add_assist_npc(&mut s, 2000, 7, 50.0);
    s.add_aggressor(2000, 10);
    s.brains.get_mut(&2000).unwrap().add_effect(12, 1500.0);
    s.actor_combat.get_mut(&2000).unwrap().condition.now = [40.0, 30.0, 20.0];
    let cleared = Arc::new(AtomicUsize::new(0));
    let counter = cleared.clone();
    let group = s.spawn_group(vec![1000, 2000], move |_| { counter.fetch_add(1, Ordering::SeqCst); });
    assert!(s.mark_npc_group_sleepable(group, NpcSpawnOrigin::Belt(77)));
    assert!(s.remove_ball(1000).is_some());
    assert_eq!((s.group_remaining(group), cleared.load(Ordering::SeqCst)), (1, 0));
    let payload = s.take_sleeping_npcs().unwrap().expect("proven cohort");
    assert_eq!((payload.survivors.len(), payload.survivors[0].id, payload.survivors[0].pos), (1, 2000, [50.0, 0.0, 0.0]));
    assert!(payload.brains.get(&2000).is_some_and(|b| b.aggressors.contains(&10) && b.modules.len() == 1));
    assert_eq!(payload.actor_combat[&2000].condition.now, [40.0, 30.0, 20.0]);
    let mut s2 = System::new(7, Arc::new(Pilots::default()), 0);
    s2.projection_sde = Some(crate::sde::types::tests::combat_fixture());
    s2.install_sleeping_npcs(payload);
    assert_eq!((s2.ball(2000).unwrap().pos, s2.group_remaining(group)), ([50.0, 0.0, 0.0], 1));
    assert!(s2.is_npc(2000) && s2.ball_token(2000).is_some());
    assert_eq!((s2.actor_combat[&2000].condition.now, s2.brains[&2000].aggressors.contains(&10)), ([40.0, 30.0, 20.0], true));
    assert_eq!(cleared.load(Ordering::SeqCst), 0);   // sleep never reads as cleared
    assert!(s2.remove_ball(2000).is_some());        // the real last removal clears once
    assert_eq!(cleared.load(Ordering::SeqCst), 1);
}

#[test]
fn sleep_admission_refuses_live_activity_and_unknown_groups() {
    let mut s = cohort();
    assert!(!s.mark_npc_group_sleepable(999_999, NpcSpawnOrigin::Belt(77)));
    let group = *s.groups.keys().next().unwrap();
    assert!(!s.mark_npc_group_sleepable(group, NpcSpawnOrigin::Belt(78)));
    // Each defect refuses without moving any brain, ball or group.
    assert_eq!(blocked_with(|s| { s.member_groups.get_mut(&1000).unwrap().insert(9_999); }), Some("npc_group_membership"));
    assert_eq!(cohort_with(|b| b.vel = [1.0, 0.0, 0.0]).npc_sleep_blocked(), Some("npc_velocity"));
    assert_eq!(cohort_with(|b| b.cloak = 1).npc_sleep_blocked(), Some("npc_ball"));
    assert_eq!(cohort_with(|b| b.mode = MODE_GOTO).npc_sleep_blocked(), Some("npc_ball"));
    assert_eq!(cohort_with(|b| b.launcher = 5).npc_sleep_blocked(), Some("npc_ball"));
    // The sampled-segment refusal shares this path but is not
    // fixture-constructible; every public mutation re-anchors interpolation.
    assert_eq!(blocked_with(|s| s.brains.get_mut(&1000).unwrap().config.repairs.0 = 5.0), Some("npc_repair"));
    assert_eq!(blocked_with(|s| s.brains.get_mut(&1000).unwrap().config.repairs.0 = f64::NAN), Some("npc_repair"));
    assert_eq!(blocked_with(|s| s.brains.get_mut(&1000).unwrap().target = Some((42, 10, 0))), Some("npc_target"));
    assert_eq!(blocked_with(|s| s.brains.get_mut(&1000).unwrap().announced = true), Some("npc_target"));
    assert_eq!(blocked_with(|s| s.actor_combat.get_mut(&1000).unwrap().phase = combat::CombatPhase::Dying { token: 1 }), Some("actor_combat"));
    assert_eq!(blocked_with(|s| s.actor_combat.get_mut(&1000).unwrap().phase = combat::CombatPhase::Transition { token: 1 }), Some("actor_combat"));
    assert_eq!(blocked_with(|s| s.actor_combat.get_mut(&1000).unwrap().character_timer = Some((1, 0))), Some("actor_combat"));
    assert_eq!(blocked_with(|s| install_npc_condition(s, 2000)), Some("actor_combat"));
    // A member with a ball but no brain cannot be marked or slept.
    s.insert(ball(2000, None, 20.0));
    let unbrained = s.spawn_group(vec![2000], |_| {});
    assert!(!s.mark_npc_group_sleepable(unbrained, NpcSpawnOrigin::Belt(77)));
    assert_eq!(s.npc_sleep_blocked(), Some("npc_group_origin"));
    // An overlapping marked member still contacts the first and refuses.
    let mut s = cohort();
    s.insert(ball(2000, None, 0.5));
    install_npc_condition(&mut s, 2000);
    assert!(s.add_npc(2000, brain(None)));
    let overlapping = s.spawn_group(vec![2000], |_| {});
    assert!(s.mark_npc_group_sleepable(overlapping, NpcSpawnOrigin::Belt(77)));
    assert_eq!(s.npc_sleep_blocked(), Some("collision"));
}

#[test]
fn combat_facts_pending_attempts_block_sleep_and_ledger_continues() {
    let mut s = cohort();
    let mut facts = CombatFacts::new(9, s.id, 8, 8);
    let activation = { let mut a = [0u8; 16]; a[8..].copy_from_slice(&7u64.to_be_bytes()); a };
    facts.register_source(activation, 7, 1).unwrap();
    let entity = crate::lua::events::combat::CombatEntity {
        item_id: 1000, type_id: 1, incarnation: [0u8; 16],
        character_id: None, corporation_id: None, alliance_id: None, faction_id: None };
    let context = crate::lua::events::resources::FactContext {
        corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(1000),
        ship_type_id: Some(1), system_id: Some(7), station_id: None,
        origin: crate::lua::events::resources::FactOrigin::Natural };
    let source = CombatSource { actual_actor_character: None, entity, source_is_npc: true,
        context, controller_incarnation: None, module_id: None, effect_id: None,
        weapon_type_id: None, activation, sequence: 1 };
    facts.reserve_attempt(&source).unwrap();
    s.combat_facts = Some(facts);
    // A clean journal with an outstanding attempt is not quiescent.
    assert_eq!(s.npc_sleep_blocked(), Some("combat_attempts"));
    let facts = s.combat_facts.as_mut().unwrap();
    let reserved = facts.reserve_damage(source, &entity, 1).unwrap();
    facts.release(reserved);
    assert!(!facts.has_pending_attempts());
    let payload = s.take_sleeping_npcs().unwrap().expect("proven cohort");
    // A cold ball colliding with a stored ID keeps the payload unconsumed.
    let mut conflict = System::new(7, Arc::new(Pilots::default()), 0);
    conflict.insert(ball(payload.survivors[0].id, None, 0.0));
    assert_eq!(conflict.sleeping_npcs_install_blocked(&payload), Some("npc_sleep_conflict"));
    let mut s2 = System::new(7, Arc::new(Pilots::default()), 0);
    s2.projection_sde = Some(crate::sde::types::tests::combat_fixture());
    s2.install_sleeping_npcs(payload);
    let restored = s2.combat_facts.as_mut().unwrap();
    assert_eq!(restored.runtime_epoch(), 9);
    assert!(restored.reserve_attempt(&CombatSource { sequence: 2, ..source }).is_ok());
    assert!(matches!(restored.reserve_attempt(&CombatSource { sequence: 1, ..source }), Err(Refusal::Replay)));
}
