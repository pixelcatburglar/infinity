use super::*;

pub(crate) fn cached_fitting(s: &System, ship: i64, module: i64, attr: i32) -> Option<(f64, Option<(bool, bool)>)> {
    let fit = &s.actor_combat.get(&ship)?.fit;
    Some((fit.value(Key::Ship, attr), fit.module_state(module)))
}

impl System {
    pub(crate) fn dying_snapshot(&self, ship: i64) -> Option<Arc<FirstLethalSnapshot>> {
        match self.actor_combat.get(&ship)?.pending.as_ref()? {
            PendingCombat::Dying { snapshot, .. } => Some(Arc::clone(snapshot)),
            PendingCombat::Transition { .. } => None,
        }
    }

    pub(crate) fn pending_reconciliations(&self) -> Vec<(i64, u64, u64)> {
        self.actor_combat.iter().filter_map(|(&ship, record)| {
            record.pending.as_ref().map(|pending| (ship, record.membership_generation,
                match pending {
                    PendingCombat::Dying { token, .. } | PendingCombat::Transition { token, .. } => *token,
                }))
        }).collect()
    }
}

fn condition(now: [f64; 3]) -> Condition {
    Condition {
        stats: ShipStats { max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 1e30 },
        now,
        shield_at: filetime_now() + FILETIME_SECOND,
    }
}
#[test]
fn combat_clock_never_regresses_before_actor_tick() {
    let mut system = System::default();
    let future = filetime_now().saturating_add(FILETIME_SECOND);
    system.nativefact_clock = future;
    assert_eq!(system.combat_fact_time(), future);
    assert_eq!(system.combat_fact_time(), future);
    assert_eq!(system.now_ms(), 0);
}


#[test]
fn typed_damage_spills_once_and_preserves_existing_overdamage() {
    let mut c = condition([10.0, 20.0, 30.0]);
    let layers = absorb_preserving_overdamage(&mut c, [35.0, 0.0, 0.0, 0.0]);
    assert_eq!(layers, [10.0, 20.0, 5.0]);
    assert_eq!(c.now, [0.0, 0.0, 25.0]);
    let mut c = condition([0.0, -30.0, 30.0]);
    assert_eq!(absorb_preserving_overdamage(&mut c, [5.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 5.0]);
    assert_eq!(c.now, [0.0, -30.0, 25.0]);
}

#[test]
fn rebase_retains_overdamage_and_unchanged_stats_do_not_reanchor() {
    let mut c = condition([80.0, 50.0, 70.0]);
    let original = c.stats.clone();
    let anchor = c.shield_at;
    assert!(!rebase_condition(&mut c, original.clone()));
    assert_eq!(c.shield_at, anchor);
    let mut smaller = original.clone();
    smaller.max = [60.0, 100.0, 100.0];
    assert!(rebase_condition(&mut c, smaller));
    assert_eq!(c.now, [60.0, -50.0, -130.0]);
    assert!(rebase_condition(&mut c, original));
    assert!((c.now[0] - 60.0).abs() < 1e-9);
    assert_eq!(&c.now[1..], &[50.0, 70.0]);
}

#[test]
fn rebase_samples_shield_at_old_rate_and_new_anchor() {
    let mut c = condition([25.0, 100.0, 200.0]);
    c.stats.shield_tau_ms = 10_000.0;
    c.shield_at = filetime_now() - FILETIME_SECOND;
    let old = c.clone();
    let mut stats = old.stats.clone();
    stats.shield_tau_ms = 100_000.0;
    let before = filetime_now();
    assert!(rebase_condition(&mut c, stats));
    let after = filetime_now();
    assert!((before..=after).contains(&c.shield_at));
    let elapsed = (c.shield_at - old.shield_at) as f64 / FILETIME_SECOND as f64;
    let expected = recharge(old.now[0], old.stats.max[0], old.stats.shield_tau_ms, elapsed);
    assert!((c.now[0] - expected).abs() < 1e-9);
    assert_eq!(&c.now[1..], &old.now[1..]);
}

#[test]
fn fitted_maxima_and_resistance_preserve_condition() {
    let mut c = condition([80.0, 150.0, 210.0]);
    let original = c.stats.clone();
    let mut changed = original.clone();
    changed.max = [60.0, 300.0, 400.0];
    changed.resonance[0][0] = 0.5;
    assert!(rebase_condition(&mut c, changed));
    assert_eq!(c.now, [60.0, 250.0, 310.0]);
    assert_eq!(absorb_preserving_overdamage(&mut c, [20.0, 0.0, 0.0, 0.0]), [10.0, 0.0, 0.0]);
    assert_eq!(c.now[0], 50.0);
    assert!(rebase_condition(&mut c, original.clone()));
    assert!((c.now[0] - 50.0).abs() < 1e-9);
    assert_eq!(&c.now[1..], &[150.0, 210.0]);
    assert_eq!(absorb_preserving_overdamage(&mut c, [20.0, 0.0, 0.0, 0.0]), [20.0, 0.0, 0.0]);
    let mut boundary = original;
    boundary.max = [20.0, 50.0, 90.0];
    assert!(rebase_condition(&mut c, boundary));
    assert_eq!(c.now, [20.0, 0.0, 0.0]);
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn hub_damage_immunity_covers_single_and_area_native_paths() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        if ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        stats.max.iter().all(|x| x.is_finite() && *x > 10.0)
            && stats.resonance.iter().all(|layer| layer[0].is_finite() && (0.0..=1.0).contains(&layer[0]) && layer[0] > 0.0)
    }).unwrap().type_id;
    let fit = Fit::for_space_type(sde.clone(), ty, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let ball = crate::space::Ball {
        id: 1, type_id: ty, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: Some(2),
        signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        goto: [0.0; 3], follow_id: 0, old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, follow_range: 0.0, free: true, global: false, massive: true,
        interactive: true, warp: crate::space::WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    let mut system = System::default();
    system.insert(ball.clone());
    system.install_condition(1, Condition { now: stats.max, stats: stats.clone(), shield_at: filetime_now() }, fit);
    let before_force = system.actor_combat[&1].condition.now;
    let force_revision = system.actor_combat[&1].revision;
    system.sovereignty.hub = Some(1);
    let force_source = DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() };
    assert!(system.apply_damage(1, [0.0; 4], force_source, true).unwrap().is_none());
    assert_eq!(system.actor_combat[&1].condition.now, before_force);
    assert_eq!(system.actor_combat[&1].revision, force_revision);
    assert_eq!(system.combat_phase(1), Some(CombatPhase::Live));
    assert!(system.combat_facts.is_none());
    system.sovereignty.hub = None;
    let mut source_ball = ball;
    source_ball.id = 3;
    source_ball.char_id = Some(3);
    system.insert(source_ball);
    system.install_condition(3, Condition { now: stats.max, stats, shield_at: filetime_now() },
        Fit::for_space_type(sde, ty, Vec::new()));
    let activation = crate::space::targeting::next_generation();
    let facts = system.prepare_combat_source(3, None, None, Some(4),
        crate::lua::events::resources::FactOrigin::Natural, activation, 1, 1, |_| true).unwrap();
    let source = DamageSource { attacker_id: Some(3), facts: Some(facts), ..Default::default() };
    system.observe_combat_utc().unwrap();
    let at = system.combat_observed_time();
    let raw = [1.0, 0.0, 0.0, 0.0];
    assert!(system.plan_damage(1, raw, source.clone(), false, at).is_some());
    assert!(system.plan_area_damage(1, raw, source.clone(), at).is_some());
    let before = system.actor_combat[&1].condition.now;
    let revision = system.actor_combat[&1].revision;
    assert!(!system.combat_facts.as_ref().unwrap().is_dirty());
    system.sovereignty.hub = Some(1);
    assert!(system.apply_damage(1, raw, source.clone(), false).unwrap().is_none());
    assert_eq!(system.actor_combat[&1].condition.now, before);
    assert_eq!(system.actor_combat[&1].revision, revision);
    assert_eq!(system.combat_phase(1), Some(CombatPhase::Live));
    assert!(!system.combat_facts.as_ref().unwrap().is_dirty());
    assert!(system.plan_area_damage(1, raw, source, system.combat_observed_time()).is_none());
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn actor_dirty_ack_and_lethal_rollback_fences() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        if ty.category_id != Some(infinity_core::consts::CATEGORY_SHIP) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        stats.max.iter().all(|x| x.is_finite() && *x > 10.0)
            && stats.resonance.iter().all(|layer| layer[0].is_finite() && (0.0..=1.0).contains(&layer[0]) && layer[0] > 0.0)
    }).unwrap().type_id;
    let fit = Fit::for_space_type(sde.clone(), ty, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let ball = crate::space::Ball {
        id: 1, type_id: ty, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: Some(2),
        signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        goto: [0.0; 3], follow_id: 0, old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, follow_range: 0.0, free: true, global: false, massive: true,
        interactive: true, warp: crate::space::WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    let mut system = System::default();
    system.insert(ball.clone());
    system.install_condition(1, Condition { now: stats.max, stats: stats.clone(), shield_at: filetime_now() }, fit);
    for (id, character) in [(3, Some(3)), (4, None)] {
        let mut source_ball = ball.clone();
        source_ball.id = id;
        source_ball.char_id = character;
        system.insert(source_ball);
        system.install_condition(id, Condition { now: stats.max, stats: stats.clone(), shield_at: filetime_now() },
            Fit::for_space_type(sde.clone(), ty, Vec::new()));
    }
    let activation = crate::space::targeting::next_generation();
    assert!(system.capture_dirty_condition(1, true, system.combat_observed_time()).is_none());
    assert!(!system.repair_condition(1, 1, 0.0, RepairSource::Local).unwrap());
    let make = |system: &mut System, item, sequence| system.prepare_combat_source(item, None, None, Some(4),
        crate::lua::events::resources::FactOrigin::Natural, activation, sequence, 1, |_| true).unwrap();
    let first = DamageSource { attacker_id: Some(3), illegal: true, facts: Some(make(&mut system, 3, 1)), ..Default::default() };
    assert_eq!(system.apply_damage(1, [0.0; 4], first.clone(), false).unwrap().unwrap().dealt, 0.0);
    assert!(system.capture_dirty_condition(1, true, system.combat_observed_time()).is_none());
    let first = DamageSource { facts: Some(make(&mut system, 3, 2)), ..first };
    let hit = system.apply_damage(1, [1.0, 0.0, 0.0, 0.0], first.clone(), false).unwrap().unwrap();
    let after_hit = system.actor_combat[&1].condition.now;
    assert!(matches!(system.apply_damage(1, [1.0, 0.0, 0.0, 0.0], first, false), Err(crate::space::combat_facts::Refusal::Replay)));
    assert_eq!(system.actor_combat[&1].condition.now, after_hit);
    assert!(hit.dealt > 0.0 && !hit.lethal);
    assert!(system.capture_dirty_condition(1, false, system.combat_observed_time()).is_none());
    let captured = system.capture_dirty_condition(1, true, system.combat_observed_time()).unwrap();
    let second = DamageSource { attacker_id: Some(3), weapon_type_id: Some(4), facts: Some(make(&mut system, 3, 3)), ..Default::default() };
    assert!(system.apply_damage(1, [1.0, 0.0, 0.0, 0.0], second.clone(), false).unwrap().unwrap().dealt > 0.0);
    assert!(!system.acknowledge_condition(1, captured.generation, captured.revision));
    let current = system.capture_dirty_condition(1, true, system.combat_observed_time()).unwrap();
    assert!(system.acknowledge_condition(1, current.generation, current.revision));
    assert!(system.capture_dirty_condition(1, true, system.combat_observed_time()).is_none());
    let mut expired = system.actor_combat[&1].contributions[&3].clone();
    expired.attacker_id = 5;
    expired.last_damage_at = 0;
    system.actor_combat.get_mut(&1).unwrap().contributions.insert(5, expired);
    system.wake_pending_combat();
    assert!(!system.actor_combat[&1].contributions.contains_key(&5));
    assert!(system.actor_combat[&1].contributions.contains_key(&3));
    assert!(system.mark_condition_dirty(1, current.generation));
    let second = DamageSource { facts: Some(make(&mut system, 3, 4)), ..second };
    let lethal = system.apply_damage(1, [1e30, 0.0, 0.0, 0.0], second, false).unwrap().unwrap();
    let snapshot = lethal.first_lethal.unwrap();
    assert!(snapshot.pending_damage.is_some());
    assert_eq!(snapshot.victim.item_id, 1);
    let before_death = system.capture_dirty_condition(1, true, system.combat_observed_time()).unwrap();
    assert_eq!(before_death.revision, snapshot.before_revision);
    assert_eq!(before_death.sampled_at, snapshot.lethal_at);
    assert_eq!(before_death.hull_damage, snapshot.condition.stats.max[2] - snapshot.condition.now[2]);
    assert!(!system.acknowledge_condition(1, before_death.generation, before_death.revision));
    assert!(lethal.lethal);
    assert_eq!(snapshot.source.weapon_type_id, Some(4));
    let stored = system.dying_snapshot(1).expect("stored first-lethal snapshot");
    assert!(Arc::ptr_eq(&snapshot, &stored));
    assert_eq!(stored.source.attacker_id, Some(3));
    assert_eq!(stored.source.weapon_type_id, Some(4));
    assert_eq!(system.pending_reconciliations(), vec![(1, snapshot.generation, snapshot.token)]);
    assert_eq!(snapshot.contributions.len(), 1);
    assert!(snapshot.contributions[0].illegal);
    let lethal_damage = snapshot.contributions[0].damage;
    system.actor_combat.get_mut(&1).unwrap().contributions.get_mut(&3).unwrap().last_damage_at = 0;
    system.wake_pending_combat();
    assert!(system.actor_combat[&1].contributions.is_empty());
    assert_eq!(snapshot.contributions.len(), 1);
    assert_eq!(snapshot.contributions[0].damage, lethal_damage);
    assert!(system.apply_damage(1, [1.0; 4], DamageSource::default(), false).unwrap().is_none());
    assert!(!system.repair_condition(1, 1, 1.0, RepairSource::Local).unwrap());
    assert!(!system.restore_known_rollback(1, snapshot.token.wrapping_add(1), snapshot.generation));
    assert!(!system.finalize_dying(1, snapshot.token, snapshot.generation.wrapping_add(1)));
    assert!(system.restore_known_rollback(1, snapshot.token, snapshot.generation));
    assert_eq!(system.combat_phase(1), Some(CombatPhase::Live));
    let restored = system.combat_condition_snapshot(1).unwrap();
    assert_eq!(restored.contributions.len(), 1);
    assert_eq!(restored.contributions[0].damage, snapshot.undo_contributions[0].damage);
    assert!(restored.contributions[0].illegal);
    system.actor_combat.get_mut(&1).unwrap().contributions.get_mut(&3).unwrap().last_damage_at = 0;
    assert!(system.combat_condition_snapshot(1).unwrap().contributions.is_empty());
    assert!(system.actor_combat[&1].contributions.is_empty());
    assert!(system.capture_dirty_condition(1, true, system.combat_observed_time()).is_some());
    let (token, transition) = system.begin_combat_transition(1).unwrap();
    assert!(system.apply_damage(1, [1.0; 4], DamageSource::default(), false).unwrap().is_none());
    assert!(!system.repair_condition(1, 1, 1.0, RepairSource::Local).unwrap());
    assert!(!system.cancel_combat_transition(1, token.wrapping_add(1), transition.generation));
    assert!(!system.commit_combat_transition(1, token.wrapping_add(1)));
    assert!(system.cancel_combat_transition(1, token, transition.generation));
    assert_eq!(system.combat_phase(1), Some(CombatPhase::Live));
    let old_capture = system.capture_dirty_condition(1, true, system.combat_observed_time()).unwrap();
    system.take(1).unwrap();
    system.insert(ball.clone());
    let fit = Fit::for_space_type(sde.clone(), ty, Vec::new());
    system.install_condition(1, Condition { now: stats.max, stats: stats.clone(), shield_at: filetime_now() }, fit);
    let generation = system.combat_membership_generation(1).unwrap();
    assert_ne!(generation, old_capture.generation);
    assert!(system.mark_condition_dirty(1, generation));
    assert!(!system.acknowledge_condition(1, old_capture.generation, old_capture.revision));
    assert!(system.capture_dirty_condition(1, true, system.combat_observed_time()).is_some());
    assert!(system.set_condition_fraction_mask(1, 0b010, 0.25));
    let original = system.actor_combat[&1].condition.stats.clone();
    let mut smaller = original.clone();
    smaller.max[1] *= 0.1;
    assert!(rebase_condition(&mut system.actor_combat.get_mut(&1).unwrap().condition, smaller));
    let before = system.actor_combat[&1].condition.now[1];
    assert!(before < 0.0);
    let repair = original.max[1] * 0.1;
    assert!(system.repair_condition(1, 1, repair, RepairSource::Local).unwrap());
    let repaired = system.actor_combat[&1].condition.now[1];
    assert!(repaired < 0.0);
    assert!((repaired - (before + repair)).abs() < 1e-9);
    assert!(rebase_condition(&mut system.actor_combat.get_mut(&1).unwrap().condition, original.clone()));
    assert!((system.actor_combat[&1].condition.now[1] - original.max[1] * 0.35).abs() < 1e-8);
    assert!(system.set_condition_fraction_mask(1, 0b100, 0.5));
    let remote = system.prepare_combat_source(3, Some(20), Some(30), None,
        crate::lua::events::resources::FactOrigin::Natural, activation, 5, 1, |_| true).unwrap();
    assert!(system.repair_condition(1, 2, 1e30, RepairSource::Remote(remote)).unwrap());
    assert_eq!(system.actor_combat[&1].condition.now[2], original.max[2]);
    assert!(!system.repair_condition(1, 2, 1.0, RepairSource::Remote(remote)).unwrap());
    let npc_activation = crate::space::targeting::next_generation();
    let npc = DamageSource { source_ship_type: Some(ty), weapon_type_id: Some(4),
        facts: Some(system.prepare_combat_source(4, None, None, Some(4),
            crate::lua::events::resources::FactOrigin::Natural, npc_activation, 1, 1, |_| true).unwrap()), ..Default::default() };
    let final_hit = system.apply_damage(1, [1e30, 0.0, 0.0, 0.0], npc, false).unwrap().unwrap();
    let death = final_hit.first_lethal.unwrap();
    assert!(death.contributions.is_empty());
    assert_eq!(death.source.attacker_id, None);
    assert_eq!(death.source.source_ship_type, Some(ty));
    assert_eq!(death.source.weapon_type_id, Some(4));
    assert!(!system.finalize_dying(1, death.token.wrapping_add(1), death.generation));
    assert!(system.finalize_dying(1, death.token, death.generation));
    assert!(system.combat_phase(1).is_none());
    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(7, system.id, 2, 4));
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(3, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
    let damage_source = DamageSource { facts: Some(source), ..Default::default() };
    system.observe_combat_utc().unwrap();
    let at = system.combat_observed_time();
    let before = [system.actor_combat[&3].condition.now, system.actor_combat[&4].condition.now];
    let mut targets = vec![
        system.plan_area_damage(3, [1.0, 0.0, 0.0, 0.0], damage_source.clone(), at).unwrap(),
        system.plan_area_damage(4, [1.0, 0.0, 0.0, 0.0], damage_source, at).unwrap(),
    ];
    assert!(matches!(system.apply_area_damage(source, &mut targets, at, |_| true), Err(crate::space::combat_facts::Refusal::Full)));
    assert_eq!([system.actor_combat[&3].condition.now, system.actor_combat[&4].condition.now], before);
    let source = system.prepare_combat_source(3, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 2, 1, |_| true).unwrap();
    assert!(matches!(system.apply_damage(4, [1.0, 0.0, 0.0, 0.0],
        DamageSource { facts: Some(source), ..Default::default() }, false), Err(crate::space::combat_facts::Refusal::Full)));
    assert_eq!(system.actor_combat[&4].condition.now, before[1]);
    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(8, system.id, 6, 4));
    let generation = crate::space::targeting::next_generation();
    let source = system.prepare_combat_source(3, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
    let damage_source = DamageSource { facts: Some(source), ..Default::default() };
    system.observe_combat_utc().unwrap();
    let at = system.combat_observed_time();
    let mut targets = vec![
        system.plan_area_damage(3, [1.0, 0.0, 0.0, 0.0], damage_source.clone(), at).unwrap(),
        system.plan_area_damage(4, [1.0, 0.0, 0.0, 0.0], damage_source, at).unwrap(),
    ];
    assert!(system.apply_area_damage(source, &mut targets, at, |_| true).unwrap());
    assert!(targets.iter().all(|entry| entry.payload.outcome.as_ref().is_some_and(|outcome| outcome.dealt > 0.0)));
    system.actor_combat.get_mut(&3).unwrap().revision = u64::MAX - 1;
    assert!(system.repair_condition(3, 0, 1e30, RepairSource::Local).unwrap());
    assert_eq!(system.actor_combat[&3].revision, u64::MAX);
    assert_ne!(system.actor_combat[&3].saved_revision, u64::MAX);
    let before = system.actor_combat[&3].condition.now;
    assert!(!system.set_condition_fraction(3, 0.5));
    assert!(!system.repair_condition(3, 1, 1.0, RepairSource::Local).unwrap());
    assert_eq!(system.actor_combat[&3].condition.now, before);
    system.actor_combat.get_mut(&4).unwrap().revision = u64::MAX - 1;
    let source = system.prepare_combat_source(3, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, generation, 2, 1, |_| true).unwrap();
    let lethal = system.apply_damage(4, [1e30, 0.0, 0.0, 0.0],
        DamageSource { facts: Some(source), ..Default::default() }, false).unwrap().unwrap().first_lethal.unwrap();
    assert_eq!(system.actor_combat[&4].revision, u64::MAX);
    assert!(!system.restore_known_rollback(4, lethal.token, lethal.generation));
    assert_eq!(system.combat_phase(4), Some(CombatPhase::Dying { token: lethal.token }));
    system.combat_facts = Some(crate::space::combat_facts::CombatFacts::new(9, system.id, 16, 4));
    let mut drone = ball.clone();
    drone.id = 5; drone.char_id = None; drone.owner_id = 3;
    system.insert(drone);
    let spec = crate::space::drone::DroneSpec { type_id: ty, bandwidth: 1.0, scan_resolution: 1.0,
        control_range: 100.0, max_target_range: 100.0, optimal_range: 100.0,
        orbit_range: 1.0, duration_ms: 1000.0, effect_id: None };
    system.register_drone(5, spec, Arc::new(|_, _| Some(Box::pin(async {}))), Some(3), Some(3), false);
    system.install_condition(5, Condition { now: stats.max, stats: stats.clone(), shield_at: filetime_now() },
        Fit::for_space_type(sde.clone(), ty, Vec::new()));
    let mut victim = ball;
    victim.id = 6; victim.char_id = Some(6); victim.owner_id = 6;
    system.insert(victim);
    system.install_condition(6, Condition { now: stats.max, stats: stats.clone(), shield_at: filetime_now() },
        Fit::for_space_type(sde, ty, Vec::new()));
    let make_drone = |system: &mut System| system.prepare_body_combat_source(5, None, None, None,
        crate::lua::events::resources::FactOrigin::Natural, |_| true).unwrap();
    let source = make_drone(&mut system);
    assert_eq!(source.actual_actor_character, Some(3));
    assert_eq!(source.entity.character_id, None);
    let hit = DamageSource { attacker_id: Some(3), facts: Some(source), ..Default::default() };
    system.apply_damage(6, [1.0, 0.0, 0.0, 0.0], hit, false).unwrap().unwrap();
    let entry = &system.actor_combat[&6].contributions[&3];
    assert_eq!(entry.entity.item_id, 5);
    assert_eq!(entry.entity.character_id, None);
    assert_eq!(entry.context.ship_id, Some(3));
    let source = make_drone(&mut system);
    let hit = DamageSource { attacker_id: Some(3), facts: Some(source), ..Default::default() };
    let death = system.apply_damage(6, [1e30, 0.0, 0.0, 0.0], hit, false)
        .unwrap().unwrap().first_lethal.unwrap();
    assert_eq!(death.source.facts.unwrap().entity.item_id, 5);
    assert_eq!(death.source.facts.unwrap().entity.character_id, None);
    assert_eq!(death.contributions[0].entity.item_id, 5);
    assert_eq!(death.contributions[0].entity.character_id, None);
}
