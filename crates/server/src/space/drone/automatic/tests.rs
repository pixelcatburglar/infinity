use super::*;
use crate::services::drones::settings::{self, DroneSettings, Patch};
use crate::space::npc::{NpcBrain, NpcConfig};

fn ball(id: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: pilot.unwrap_or(1), corp_id: 1, alliance_id: -1,
        char_id: pilot, signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: pilot.is_some(),
        massive: true, warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn spec() -> DroneSpec {
    DroneSpec { type_id: 1, bandwidth: 5.0, scan_resolution: 100.0, control_range: 1000.0,
        max_target_range: 1000.0, optimal_range: 100.0, orbit_range: 50.0, duration_ms: 1000.0, effect_id: Some(1) }
}

fn setup(base: i64) -> System {
    crate::tests::init_config();
    let mut s = System::new(1, Arc::default(), 1000);
    s.insert(ball(base, Some(base + 1), 0.0));
    let token = s.reserve_drone_launch(base + 1, base, &[5.0, 5.0], 2.0, 10.0).unwrap();
    let mut first = ball(base + 10, None, 10.0); first.owner_id = base + 1;
    let mut second = ball(base + 11, None, 10.0); second.owner_id = base + 1;
    let effects: DroneEffects = Arc::new(|_, _| Some(Box::pin(async {})));
    s.cancel_drone_launch(token);
    for drone in [second, first] {
        let id = drone.id;
        s.insert(drone);
        s.register_drone(id, spec(), effects.clone(), Some(base), Some(base + 1), false);
    }
    s
}

fn npc(s: &mut System, id: i64, x: f64) {
    s.insert(ball(id, None, x));
    let fit = crate::services::fit::Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, Vec::new());
    let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, crate::services::fit::Key::Ship);
    s.install_condition_with_key(id, crate::services::combat::groups_health::Condition {
        now: stats.max, stats, shield_at: crate::client::filetime_now(),
    }, fit, crate::services::fit::Key::Ship);
    assert!(s.add_npc(id, NpcBrain::new(None, NpcConfig { fly_range: 20.0, aggro_range: 100.0,
        rate_of_fire_ms: 2500.0, scan_resolution: 100.0, repairs: (0.0, 0.0, 5000.0) },
        Arc::new(|_, _| Some(Box::pin(async {}))))));
}

fn preferences(owner: i64, aggressive: bool, focus_fire: bool) {
    settings::apply(owner, DroneSettings::default(), Patch { aggressive: Some(aggressive), focus_fire: Some(focus_fire) });
}

#[test]
fn passive_records_npc_attack_and_aggressive_idle_drones_engage() {
    let base = -720000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 20.0);
    s.drone_attack(base + 40, base);
    s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Idle);
    preferences(owner, true, true);
    s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Engage(base + 40));
    settings::logged_off(owner);
}

#[test]
fn focus_and_round_robin_use_sorted_physical_ids() {
    for (base, focus) in [(-721000, true), (-722000, false)] {
        let owner = base + 1; settings::logged_off(owner);
        let mut s = setup(base); npc(&mut s, base + 41, 20.0); npc(&mut s, base + 40, 20.0);
        s.drone_attack(base + 41, base); s.drone_attack(base + 40, base);
        preferences(owner, true, focus); s.think_drones();
        assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Engage(base + 40));
        assert_eq!(s.drone(base + 11).unwrap().order, DroneOrder::Engage(base + if focus { 40 } else { 41 }));
        settings::logged_off(owner);
    }
}

#[test]
fn explicit_orders_and_ineligible_sources_are_not_retargeted() {
    let base = -723000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 2000.0); npc(&mut s, base + 41, 20.0);
    s.insert(ball(base + 42, Some(base + 2), 20.0));
    s.drone_attack(base + 40, base); s.drone_attack(base + 41, base); s.drone_attack(base + 42, base);
    s.remove_ball(base + 41);
    assert!(s.drone_order(base + 10, owner, base, DroneOrder::Guard(owner)));
    preferences(owner, true, true); s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Guard(owner));
    assert_eq!(s.drone(base + 11).unwrap().order, DroneOrder::Idle);
    settings::logged_off(owner);
}

#[test]
fn jam_and_controller_loss_prevent_acquisition() {
    let base = -724000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 20.0);
    s.drone_attack(base + 40, base); preferences(owner, true, true);
    crate::services::combat::jamming::apply(&mut s, base + 40, base + 50, base, 10000.0);
    s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Idle);
    s.drone_controller_lost(base);
    assert!(!s.drone_aggressors.contains_key(&owner));
    settings::logged_off(owner);
}

#[test]
fn dead_and_cloaked_aggressors_are_excluded() {
    let base = -725000; let owner = base + 1;
    settings::logged_off(owner);
    let mut s = setup(base); npc(&mut s, base + 40, 20.0); npc(&mut s, base + 41, 20.0);
    s.drone_attack(base + 40, base); s.drone_attack(base + 41, base);
    s.command(base + 40, |ball, _, _| { ball.health[2] = 0.0; Vec::new() });
    s.command(base + 41, |ball, _, _| { ball.cloak = 1; Vec::new() });
    s.tick(2000);
    preferences(owner, true, true); s.think_drones();
    assert_eq!(s.drone(base + 10).unwrap().order, DroneOrder::Idle);
    assert_eq!(s.drone(base + 11).unwrap().order, DroneOrder::Idle);
    settings::logged_off(owner);
}

fn install_combat(s: &mut System, id: i64) {
    let fit = crate::services::fit::Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, Vec::new());
    let stats = crate::services::combat::groups_health::ShipStats {
        max: [100.0; 3], resonance: [[1.0; 4]; 3], shield_tau_ms: 1e30,
    };
    s.install_condition(id, crate::services::combat::groups_health::Condition {
        now: [99.0; 3], stats, shield_at: crate::client::filetime_now() + infinity_core::consts::FILETIME_SECOND,
    }, fit);
}

#[test]
fn native_remote_repair_captures_capped_layers_and_local_is_not_a_fact() {
    use crate::lua::events::{GameEvent, combat::{BinaryAmount, HpLayer}, resources::FactOrigin};
    use crate::space::combat::RepairSource;
    for (layer, expected) in [(0, HpLayer::Shield), (1, HpLayer::Armor), (2, HpLayer::Hull)] {
        let base = 730_000 + layer as i64 * 100;
        let mut s = setup(base);
        install_combat(&mut s, base);
        install_combat(&mut s, base + 10);
        npc(&mut s, base + 40, 20.0);
        s.actor_combat.get_mut(&(base + 40)).unwrap().condition.now = [99.0; 3];
        s.actor_combat.get_mut(&(base + 40)).unwrap().condition.shield_at = crate::client::filetime_now() + infinity_core::consts::FILETIME_SECOND;
        let source = s.prepare_body_combat_source(base + 10, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
        assert_eq!(source.actual_actor_character, Some(base + 1));
        assert_eq!(source.context.ship_id, Some(base));
        assert!(s.repair_condition(base + 40, layer, 0.25, RepairSource::Local).unwrap());
        assert!(!s.combat_facts.as_ref().unwrap().is_dirty());
        assert!(s.repair_condition(base + 40, layer, 10.0, RepairSource::Remote(source)).unwrap());
        assert_eq!(s.actor_combat[&(base + 40)].condition.now[layer], 100.0);
        let at = s.combat_fact_time();
        let batch = s.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
        let events = batch.events();
        assert_eq!(events.len(), 1);
        match &events[0].event {
            GameEvent::RemoteRepairApplied { char_id, source: physical, target, layer, slices, .. } => {
                assert_eq!(*char_id, Some(base + 1));
                assert_eq!(physical.item_id, base + 10);
                assert_eq!(target.item_id, base + 40);
                assert_eq!(*layer, expected);
                assert_eq!(slices[0].amount, BinaryAmount::from_f64(0.75).unwrap());
                assert_eq!(slices[0].successful_count, 1);
            }
            _ => panic!("expected one remote repair fact"),
        }
    }
}

#[test]
fn native_repair_full_refuses_before_mutation_but_local_still_repairs() {
    use crate::lua::events::resources::FactOrigin;
    use crate::space::{combat::RepairSource, combat_facts::{CombatFacts, Refusal}};
    let base = 731_000;
    let mut s = setup(base);
    install_combat(&mut s, base);
    install_combat(&mut s, base + 10);
    npc(&mut s, base + 40, 20.0);
    npc(&mut s, base + 41, 20.0);
    s.actor_combat.get_mut(&(base + 40)).unwrap().condition.now[1] = 90.0;
    s.actor_combat.get_mut(&(base + 41)).unwrap().condition.now[1] = 90.0;
    s.combat_facts = Some(CombatFacts::new(7, 1, 1, 4));
    let first = s.prepare_body_combat_source(base + 10, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
    assert!(s.repair_condition(base + 40, 1, 1.0, RepairSource::Remote(first)).unwrap());
    let second = s.prepare_body_combat_source(base + 10, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
    let before = s.actor_combat[&(base + 41)].condition.now;
    assert!(matches!(s.repair_condition(base + 41, 1, 1.0, RepairSource::Remote(second)), Err(Refusal::Full)));
    assert_eq!(s.actor_combat[&(base + 41)].condition.now, before);
    assert!(s.repair_condition(base + 41, 1, 1.0, RepairSource::Local).unwrap());
    assert_eq!(s.actor_combat[&(base + 41)].condition.now[1], before[1] + 1.0);
    let refusal = crate::services::combat::capacity::refusal(1);
    assert!(crate::services::combat::capacity::is_refusal(&refusal));
    assert!(matches!(refusal, crate::services::CallError::User { .. }));
}

#[test]
fn drone_repair_freezes_source_and_controller_but_samples_target_at_impact() {
    use crate::lua::events::{GameEvent, resources::FactOrigin};
    use crate::space::combat::RepairSource;
    let base = 732_000;
    let mut s = setup(base);
    install_combat(&mut s, base);
    install_combat(&mut s, base + 10);
    npc(&mut s, base + 40, 20.0);
    s.actor_combat.get_mut(&(base + 40)).unwrap().condition.now[1] = 90.0;
    let source = s.prepare_body_combat_source(base + 10, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
    let controller_incarnation = source.controller_incarnation;
    s.command(base, |ball, _, _| { ball.corp_id = 42; ball.alliance_id = 43; Vec::new() });
    s.command(base + 10, |ball, _, _| { ball.corp_id = 44; Vec::new() });
    s.command(base + 40, |ball, _, _| { ball.corp_id = 45; Vec::new() });
    s.tick(2000);
    assert_eq!(source.context.corporation_id, Some(1));
    assert_eq!(source.controller_incarnation, controller_incarnation);
    assert!(s.repair_condition(base + 40, 1, 1.0, RepairSource::Remote(source)).unwrap());
    let at = s.combat_fact_time();
    let batch = s.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap();
    let events = batch.events();
    match &events[0].event {
        GameEvent::RemoteRepairApplied { char_id, context, source: physical, target, .. } => {
            assert_eq!(*char_id, Some(base + 1));
            assert_eq!(context.corporation_id, Some(1));
            assert_eq!(context.ship_id, Some(base));
            assert_eq!(physical.corporation_id, Some(1));
            assert_eq!(target.corporation_id, Some(45));
        }
        _ => panic!("expected remote repair fact"),
    }
}

#[test]
fn moving_outside_action_range_discards_paused_repair_cast() {
    use crate::lua::events::resources::FactOrigin;
    let base = 733_000;
    let mut s = setup(base);
    install_combat(&mut s, base);
    install_combat(&mut s, base + 10);
    npc(&mut s, base + 40, 20.0);
    assert!(s.drone_order(base + 10, base + 1, base, DroneOrder::Engage(base + 40)));
    let generation = s.drone(base + 10).unwrap().generation;
    let facts = s.prepare_body_combat_source(base + 10, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
    let token = crate::services::effects::space_source::PreparedCombatAttempt::new(facts, std::sync::Weak::new(), 1);
    s.drone_store_combat_attempt(base + 10, generation, token);
    s.command(base + 40, |ball, _, _| { ball.pos = [200.0, 0.0, 0.0]; ball.old_pos = ball.pos; Vec::new() });
    s.tick(2000);
    s.think_drones();
    assert!(s.drone_take_combat_attempt(base + 10, generation).is_none());
    assert!(!s.combat_facts.as_ref().unwrap().is_dirty());
    s.finish_combat_attempt(&facts);
}

#[test]
fn npc_remote_repair_journals_without_a_fabricated_pilot() {
    use crate::lua::events::{GameEvent, resources::FactOrigin};
    use crate::space::combat::RepairSource;
    let base = 734_000;
    let mut s = setup(base);
    npc(&mut s, base + 40, 20.0);
    npc(&mut s, base + 41, 20.0);
    s.actor_combat.get_mut(&(base + 41)).unwrap().condition.now[2] = 90.0;
    let source = s.prepare_body_combat_source(base + 40, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
    assert!(source.source_is_npc);
    assert_eq!(source.actual_actor_character, None);
    assert_eq!(source.controller_incarnation, None);
    assert!(s.repair_condition(base + 41, 2, 2.0, RepairSource::Remote(source)).unwrap());
    let at = s.combat_fact_time();
    let events = s.combat_facts.as_mut().unwrap().capture(at).unwrap().unwrap().events();
    match &events[0].event {
        GameEvent::RemoteRepairApplied { char_id, context, source, target, layer, slices, .. } => {
            assert_eq!(*char_id, None);
            assert_eq!(context.ship_id, None);
            assert_eq!(source.item_id, base + 40);
            assert_eq!(target.item_id, base + 41);
            assert_eq!(*layer, crate::lua::events::combat::HpLayer::Hull);
            assert_eq!(slices[0].amount, crate::lua::events::combat::BinaryAmount::from_f64(2.0).unwrap());
        }
        _ => panic!("expected NPC remote repair fact"),
    }
}

#[test]
fn target_loss_discards_pending_drone_repair() {
    use crate::lua::events::resources::FactOrigin;
    let base = 735_000;
    let mut s = setup(base);
    install_combat(&mut s, base);
    install_combat(&mut s, base + 10);
    npc(&mut s, base + 40, 20.0);
    assert!(s.drone_order(base + 10, base + 1, base, DroneOrder::Engage(base + 40)));
    let generation = s.drone(base + 10).unwrap().generation;
    let facts = s.prepare_body_combat_source(base + 10, None, Some(1), None, FactOrigin::Natural, |_| true).unwrap();
    let token = crate::services::effects::space_source::PreparedCombatAttempt::new(facts, std::sync::Weak::new(), 1);
    s.drone_store_combat_attempt(base + 10, generation, token);
    s.remove_ball(base + 40);
    s.think_drones();
    assert!(s.drone_take_combat_attempt(base + 10, generation).is_none());
    assert!(!s.combat_facts.as_ref().unwrap().is_dirty());
    s.finish_combat_attempt(&facts);
}

async fn native_drone_fixture(base: i64) -> (System, DroneEffects, i32) {
    use crate::services::{drones::worker, effects::{registry, EffectKind}, fit::{Fit, Key}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().find(|ty| {
        if ty.published != Some(true) || ty.category_id != Some(infinity_core::consts::CATEGORY_DRONE) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        worker::spec(&fit, Key::Ship, ty.type_id).unwrap().effect_id
            .and_then(|id| registry::resolve(&sde, id))
            .is_some_and(|effect| matches!(effect.kind, EffectKind::Turret))
    }).expect("published turret drone").type_id;
    let mut s = setup(base);
    s.projection_sde = Some(sde.clone());
    install_combat(&mut s, base);
    s.remove_ball(base + 10);
    let mut physical = ball(base + 10, None, 10.0);
    physical.type_id = ty;
    physical.owner_id = base + 1;
    s.insert(physical);
    let fit = Fit::for_space_type(sde, ty, Vec::new());
    let mut spec = worker::spec(&fit, Key::Ship, ty).unwrap();
    spec.scan_resolution = 0.0;
    spec.control_range = 1000.0;
    spec.max_target_range = 1000.0;
    spec.optimal_range = 1000.0;
    spec.duration_ms = 1000.0;
    let callback = worker::effects(db, std::sync::Weak::new(), 1);
    s.register_drone(base + 10, spec, callback.clone(), Some(base), Some(base + 1), false);
    install_combat(&mut s, base + 10);
    npc(&mut s, base + 40, 20.0);
    (s, callback, ty)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn full_native_replay_admission_does_not_queue_drone_cycle() {
    use crate::lua::events::resources::FactOrigin;
    use crate::space::combat_facts::CombatFacts;
    let base = 736_000;
    let (mut s, callback, ty) = native_drone_fixture(base).await;
    assert!(s.drone_order(base + 10, base + 1, base, DroneOrder::Engage(base + 40)));
    s.think_drones();
    let drone = s.drone(base + 10).unwrap();
    assert!(s.drone_valid(drone.id, base + 40, drone.generation));
    s.combat_facts = Some(CombatFacts::new(7, 1, 1, 4));
    let guard = crate::services::effects::space_source::SourceGuard::Drone {
        source: drone.id, target: base + 40, generation: drone.generation,
    };
    let outstanding = s.prepare_body_combat_source(drone.id, None, None, Some(ty), FactOrigin::Natural,
        move |s| guard.valid(s)).unwrap();
    let before = s.combat_condition_snapshot(base + 40).unwrap().condition.now;
    assert!(callback(&mut s, DroneAction::Cycle { drone, target: base + 40, phase: DronePhase::Begin }).is_none());
    assert_eq!(s.combat_condition_snapshot(base + 40).unwrap().condition.now, before);
    assert!(!s.combat_facts.as_ref().unwrap().is_dirty());
    s.finish_combat_attempt(&outstanding);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn invalid_delegation_idles_before_full_native_admission() {
    use crate::lua::events::resources::FactOrigin;
    use crate::space::combat_facts::{CombatFacts, Refusal};
    use crate::services::effects::space_source::SourceGuard;
    let base = 737_000;
    let delegate = base + 2;
    let (mut s, callback, ty) = native_drone_fixture(base).await;
    s.insert(ball(base + 20, Some(delegate), 0.0));
    assert!(!crate::services::fleets::same_fleet(base + 1, delegate));
    for order in [DroneOrder::Assist(delegate), DroneOrder::Guard(delegate)] {
        assert!(s.drone_order(base + 10, base + 1, base, order));
        if matches!(order, DroneOrder::Assist(_)) { s.drone_attack(base + 20, base + 40); }
        else { s.drone_attack(base + 40, base + 20); }
        s.think_drones();
        let drone = s.drone(base + 10).unwrap();
        let guard = SourceGuard::Drone { source: drone.id, target: base + 40, generation: drone.generation };
        assert_eq!(drone.order, order);
        assert!(s.drone_valid(drone.id, base + 40, drone.generation));
        assert!(!guard.valid(&s));
        s.combat_facts = Some(CombatFacts::new(7, 1, 1, 4));
        let outstanding = s.prepare_body_combat_source(drone.id, None, None, Some(ty), FactOrigin::Natural,
            |s| s.drone_valid(drone.id, base + 40, drone.generation)).unwrap();
        assert!(matches!(s.prepare_body_combat_source(drone.id, None, drone.spec.effect_id.map(i64::from), Some(ty),
            FactOrigin::Natural, |s| s.drone_valid(drone.id, base + 40, drone.generation)), Err(Refusal::Full)));
        assert!(callback(&mut s, DroneAction::Cycle { drone, target: base + 40, phase: DronePhase::Begin }).is_none());
        let state = s.drone(base + 10).unwrap();
        s.finish_combat_attempt(&outstanding);
        assert_eq!(state.order, DroneOrder::Idle);
        assert_eq!(state.target, None);
    }
}
