use super::*;
use crate::services::combat::damage::DamageSource;
use crate::services::fit::attributes::tests::module_fit;
use crate::services::fit::reactive_armor::ARMOR_RESONANCE;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, dogma::{ATTR_ARMOR_HP, ATTR_RESISTANCE_SHIFT_AMOUNT, EFFECT_ADAPTIVE_ARMOR_HARDENER}};
use std::sync::atomic::AtomicBool;
const MODULE: i64 = 2;
struct Stage { system: System, sde: std::sync::Arc<crate::sde::Sde>, hull: i32, module_type: i32, seed: [f64; 4], rx: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<marshal::Value>)> }
async fn stage() -> Stage {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let hull_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP) && ty.attribute(ATTR_ARMOR_HP).unwrap_or(0.0) > 100.0).unwrap().type_id;
    let module_type = sde.types.iter().find(|ty| ty.effects().iter().any(|row| row.effect_id == EFFECT_ADAPTIVE_ARMOR_HARDENER) && ty.attribute(ATTR_RESISTANCE_SHIFT_AMOUNT).unwrap_or(0.0) > 0.0).unwrap().type_id;
    let fit = module_fit(sde.clone(), hull_type, module_type, true); let seed = ARMOR_RESONANCE.map(|attr| fit.modifier_value(Key::Module(MODULE), attr));
    let registry = std::sync::Arc::new(crate::space::Pilots::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new(1, registry, 0);
    let mut ball = hull(); ball.type_id = hull_type; system.add_persistent_ship(ball);
    system.insert({ let mut ball = hull(); ball.id = 3; ball.type_id = hull_type; ball.owner_id = 3; ball.char_id = Some(3); ball });
    install_fit(&mut system, 3, Fit::for_space_type(sde.clone(), hull_type, vec![]));
    resident(&mut system, &sde, hull_type, module_type);
    Stage { system, sde, hull: hull_type, module_type, seed, rx }
}
/// Install the module-bearing fit and anchor an empty, non-recharging shield.
fn resident(system: &mut System, sde: &std::sync::Arc<crate::sde::Sde>, hull: i32, module_type: i32) {
    let fit = module_fit(sde.clone(), hull, module_type, true);
    install_fit(system, 1, fit);
    let record = system.actor_combat.get_mut(&1).unwrap();
    record.condition.now[0] = 0.0; record.condition.shield_at = crate::client::filetime_now();
}
fn open(stage: &mut Stage, generation: &Arc<AtomicBool>, seed: Option<[f64; 4]>) -> u64 {
    let membership = stage.system.combat_membership_generation(1).unwrap();
    if seed.is_some() { assert!(stage.system.prepare_reactive_armor_activation(1, membership, MODULE, stage.module_type, generation)); assert!(stage.system.fitted_module_active(1, MODULE, true)); }
    assert!(stage.system.begin_reactive_armor(1, membership, MODULE, stage.module_type, generation, seed));
    membership
}
fn admit(system: &mut System, sequence: u64) -> DamageSource {
    let facts = system.prepare_combat_source(3, None, None, Some(4), crate::lua::events::resources::FactOrigin::Natural, crate::space::targeting::next_generation(), sequence, 1, |_| true).unwrap();
    DamageSource { attacker_id: Some(3), facts: Some(facts), ..Default::default() }
}
fn strike(system: &mut System, raw: [f64; 4], source: DamageSource) -> Result<Option<crate::space::combat::ActorDamageOutcome>, crate::space::combat_facts::Refusal> {
    system.apply_damage(1, raw, source, false)
}
fn hit(system: &mut System, raw: [f64; 4], sequence: u64) -> Result<Option<f64>, crate::space::combat_facts::Refusal> {
    let source = admit(system, sequence); strike(system, raw, source).map(|outcome| outcome.map(|hit| hit.dealt))
}
fn resonance(hit: &crate::space::combat::ActorDamageOutcome, system: &System, raw: f64) -> f64 {
    let shield = (hit.old[0] - hit.new[0]) / system.actor_combat[&1].condition.stats.resonance[0][0];
    (hit.old[1] - hit.new[1]) / (raw - shield)
}
fn profile(system: &System) -> [f64; 4] { ARMOR_RESONANCE.map(|attr| system.actor_combat[&1].fit.value(Key::Ship, attr)) }
fn modules(system: &System) -> [f64; 4] { ARMOR_RESONANCE.map(|attr| system.actor_combat[&1].fit.value(Key::Module(MODULE), attr)) }
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn accepted_hits_adapt_and_refused_paths_keep_the_profile() {
    let mut stage = stage().await;
    let generation = Arc::new(AtomicBool::new(false));
    let seed = stage.seed;
    let membership = open(&mut stage, &generation, Some(seed));
    let _ = attr_batches(&mut stage.rx);
    let ship_before = profile(&stage.system);
    let source = admit(&mut stage.system, 1);
    let first = strike(&mut stage.system, [10.0, 0.0, 0.0, 0.0], source.clone()).unwrap().unwrap();
    assert!(matches!(strike(&mut stage.system, [10.0, 0.0, 0.0, 0.0], source), Err(crate::space::combat_facts::Refusal::Replay)));
    assert!(first.dealt > 0.0 && first.new[0] == 0.0 && first.new[2] == first.old[2] && (resonance(&first, &stage.system, 10.0) - ship_before[0]).abs() < 1e-9);
    let (area, at) = (admit(&mut stage.system, 2), stage.system.combat_observed_time());
    let mut targets = vec![stage.system.plan_area_damage(1, [10.0, 0.0, 0.0, 0.0], area.clone(), at).unwrap()];
    assert!(stage.system.apply_area_damage(area.facts.unwrap(), &mut targets, at, |_| true).unwrap() && targets[0].payload.outcome.as_ref().unwrap().dealt > 0.0);
    let (kept, flat) = (stage.system.actor_combat[&1].condition.now, admit(&mut stage.system, 3));
    stage.system.apply_flat_damage(1, 5.0, flat).unwrap(); assert!(stage.system.actor_combat[&1].condition.now[1] < kept[1]);
    let (kept, lethal) = (stage.system.actor_combat[&1].condition.now, admit(&mut stage.system, 4));
    let lethal = stage.system.apply_damage(1, [1e30; 4], lethal, false).unwrap().unwrap().first_lethal.unwrap();
    assert!(stage.system.restore_known_rollback(1, lethal.token, lethal.generation) && stage.system.actor_combat[&1].condition.now[1..] == kept[1..]);
    assert!(stage.system.finish_reactive_armor_cycle(1, membership, MODULE, &generation, 0.06));
    let (ship_now, module_now) = (profile(&stage.system), modules(&stage.system));
    let step = |index: usize| (1.0 - stage.seed[index]).min(0.06);
    let expected = [stage.seed[0] - step(1) - step(2) - step(3), stage.seed[1] + step(1), stage.seed[2] + step(2), stage.seed[3] + step(3)];
    assert!(module_now[0] < stage.seed[0] && step(0) + step(1) + step(2) + step(3) > 0.0);
    for index in 0..4 {
        assert!((module_now[index] - expected[index]).abs() < 1e-9 && (ship_now[index] - ship_before[index] * expected[index] / stage.seed[index]).abs() < 1e-9);
    }
    assert_eq!(stage.system.actor_combat[&1].condition.stats.resonance[1], ship_now);
    let expected: Vec<_> = (0..4).flat_map(|index| [(Key::Module(MODULE), ARMOR_RESONANCE[index], stage.seed[index], module_now[index]), (Key::Ship, ARMOR_RESONANCE[index], ship_before[index], ship_now[index])]).collect();
    assert_batch(&mut stage.rx, 1, 2, Key::Ship, &expected);
    let source = admit(&mut stage.system, 5);
    let again = strike(&mut stage.system, [10.0, 0.0, 0.0, 0.0], source).unwrap().unwrap();
    assert!((resonance(&again, &stage.system, 10.0) - ship_now[0]).abs() < 1e-9 && attr_batches(&mut stage.rx).is_empty());
    assert!(open(&mut stage, &generation, None) == membership);
    assert!(stage.system.finish_reactive_armor_cycle(1, membership, MODULE, &generation, 0.06) && profile(&stage.system) == ship_now && attr_batches(&mut stage.rx).is_empty());
}
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn closed_gap_refit_offline_and_stale_retirement_keep_the_successor() {
    let mut stage = stage().await;
    let generation = Arc::new(AtomicBool::new(false));
    let seed = stage.seed;
    let membership = open(&mut stage, &generation, Some(seed));
    assert!(hit(&mut stage.system, [10.0, 0.0, 0.0, 0.0], 1).unwrap().unwrap() > 0.0);
    assert!(stage.system.finish_reactive_armor_cycle(1, membership, MODULE, &generation, 0.06));
    let adapted = profile(&stage.system);
    let _ = attr_batches(&mut stage.rx);
    assert!(hit(&mut stage.system, [10.0, 0.0, 0.0, 0.0], 2).unwrap().unwrap() > 0.0);
    assert!(open(&mut stage, &generation, None) == membership);
    assert!(stage.system.finish_reactive_armor_cycle(1, membership, MODULE, &generation, 0.06) && profile(&stage.system) == adapted && attr_batches(&mut stage.rx).is_empty());
    let before = module_fit(stage.sde.clone(), stage.hull, stage.module_type, true);
    assert!(stage.system.publish_fitting(1, 2, stage.system.persistent_generation(1).unwrap(), Fit::for_space_type(stage.sde.clone(), stage.hull, vec![]).into_committed_fitting(&before), vec![]) && stage.system.actor_combat[&1].reactive_armor.is_none());
    resident(&mut stage.system, &stage.sde, stage.hull, stage.module_type);
    let (hull_ship, successor) = (profile(&stage.system), Arc::new(AtomicBool::new(false)));
    assert!(open(&mut stage, &successor, Some(seed)) == membership);
    let other = stage.sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE) && ty.type_id != stage.module_type).unwrap().type_id;
    assert!(stage.system.publish_fitting(1, 2, stage.system.persistent_generation(1).unwrap(), module_fit(stage.sde.clone(), stage.hull, other, true).into_committed_fitting(&before), vec![]) && stage.system.actor_combat[&1].reactive_armor.is_none());
    resident(&mut stage.system, &stage.sde, stage.hull, stage.module_type); assert!(open(&mut stage, &successor, Some(seed)) == membership);
    assert!(stage.system.fitted_module_online(1, MODULE, false) && stage.system.actor_combat[&1].reactive_armor.is_none() && profile(&stage.system) == hull_ship);
    resident(&mut stage.system, &stage.sde, stage.hull, stage.module_type); assert!(open(&mut stage, &successor, Some(seed)) == membership);
    assert!(hit(&mut stage.system, [10.0, 0.0, 0.0, 0.0], 3).unwrap().unwrap() > 0.0);
    assert!(stage.system.finish_reactive_armor_cycle(1, membership, MODULE, &successor, 0.06));
    let live = profile(&stage.system);
    stage.system.retire_reactive_armor(1, membership, MODULE, &generation);
    assert!(stage.system.actor_combat[&1].reactive_armor.is_some() && profile(&stage.system) == live);
    let ball = stage.system.take(1).unwrap(); stage.system.insert(ball);
    resident(&mut stage.system, &stage.sde, stage.hull, stage.module_type);
    assert!(stage.system.actor_combat[&1].reactive_armor.is_none() && profile(&stage.system) == hull_ship);
}
