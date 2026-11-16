//! Actor fitting publication fences against stale pilots and incarnations.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::Fit;
use crate::space::{Ball, MODE_STOP};
use crate::space::tests::fit_delta::{assert_batch, attr_batches};
use std::sync::Arc;

mod drones;

fn hull() -> Ball {
    Ball { id: 1, type_id: 1, owner_id: 2, corp_id: 1, alliance_id: -1, char_id: Some(2),
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: true, warp: Default::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

#[test]
fn stale_generation_phase_and_pilot_do_not_publish() {
    crate::tests::init_config();
    let sde = crate::sde::types::tests::combat_fixture();
    let make_fit = || Fit::for_space_type(Arc::clone(&sde), 1, vec![]);
    let make_patch = || make_fit().into_committed_fitting(&make_fit());
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new  (1, registry, 0);
    system.add_persistent_ship(hull());
    let generation = system.persistent_generation(1).unwrap();
    let fit = make_fit();
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, fit);
    assert!(!system.publish_fitting(1, 2, generation.wrapping_add(1), make_patch(), vec![]));
    assert!(!system.publish_fitting(1, 3, generation, make_patch(), vec![]));
    system.actor_combat.get_mut(&1).unwrap().phase = CombatPhase::Transition { token: 1 };
    assert!(!system.publish_fitting(1, 2, generation, make_patch(), vec![]));
    assert_eq!(system.actor_combat[&1].fit_revision, 0);
    assert!(rx.try_recv().is_err());
}

#[test]
fn changed_maxima_publish_current_levels_immediately_after_each_maximum() {
    use crate::services::fit::attributes::tests::ship_fit;
    crate::tests::init_config();
    let ship = -920_001;
    let before = ship_fit(&[]);
    let final_fit = ship_fit(&[(ATTR_SHIELD_CAPACITY, 60.0), (ATTR_ARMOR_HP, 200.0),
        (ATTR_STRUCTURE_HP, 200.0), (ATTR_CAPACITOR_CAPACITY, 200.0)]);
    let patch = final_fit.into_committed_fitting(&before);
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new(1, registry, 0);
    let mut ball = hull();
    ball.id = ship;
    system.add_persistent_ship(ball);
    let generation = system.persistent_generation(ship).unwrap();
    let stats = ShipStats::from_fit(&before, Key::Ship);
    system.install_condition(ship, Condition { now: [80.0, 70.0, 50.0], stats,
        shield_at: crate::client::filetime_now() + infinity_core::consts::FILETIME_SECOND }, before);
    assert!(system.publish_fitting(ship, 2, generation, patch, vec![]));
    let (name, args) = rx.try_recv().unwrap();
    assert_eq!(name, "OnModuleAttributeChanges");
    let [Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
    let changes: Vec<_> = rows.iter().map(|row| {
        let Value::Tuple(fields) = row else { panic!("expected attribute row") };
        (fields[3].as_int().unwrap() as i32, fields[5].clone())
    }).collect();
    for (maximum, current, value) in [
        (ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE, 60.0),
        (ATTR_ARMOR_HP, ATTR_ARMOR_DAMAGE, 30.0),
        (ATTR_STRUCTURE_HP, ATTR_HULL_DAMAGE, 50.0),
        (ATTR_CAPACITOR_CAPACITY, ATTR_CHARGE, 100.0),
    ] {
        let index = changes.iter().position(|row| row.0 == maximum).unwrap();
        assert_eq!(changes[index + 1], (current, Value::Float(value)));
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn online_toggle_and_removed_modifier_publish_actor_values() {
    use crate::services::fit::attributes::tests::module_fit;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull_type = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && ShipStats::from_fit(&Fit::for_space_type(Arc::clone(&sde), ty.type_id, vec![]), Key::Ship)
            .max.iter().all(|v| v.is_finite() && *v > 0.0)).unwrap().type_id;
    let mut found = None;
    for ty in sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_MODULE)) {
        let online = module_fit(Arc::clone(&sde), hull_type, ty.type_id, true);
        let offline = module_fit(Arc::clone(&sde), hull_type, ty.type_id, false);
        let candidates = online.fitting_candidates(&offline);
        let changes = offline.fitting_changes(&online.fitting_values(&candidates));
        if changes.iter().any(|(key, _, _, _)| *key == Key::Ship) {
            found = Some(ty.type_id);
            break;
        }
    }
    let module_type = found.expect("SDE must contain an online ship modifier");
    for removed in [false, true] {
        let before = module_fit(Arc::clone(&sde), hull_type, module_type, true);
        let after = if removed { Fit::for_space_type(Arc::clone(&sde), hull_type, vec![]) }
            else { module_fit(Arc::clone(&sde), hull_type, module_type, false) };
        let expected = after.fitting_changes(&before.fitting_values(&before.fitting_candidates(&after)));
        let patch = after.into_committed_fitting(&before);
        let registry = Arc::new(crate::space::Pilots::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register_pilot(2, tx);
        let mut system = System::new(1, registry, 0);
        let mut ball = hull();
        ball.type_id = hull_type;
        system.add_persistent_ship(ball);
        let generation = system.persistent_generation(1).unwrap();
        let stats = ShipStats::from_fit(&before, Key::Ship);
        system.install_condition(1, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, before);
        assert!(system.publish_fitting(1, 2, generation, patch, vec![]));
        let (name, args) = rx.try_recv().unwrap();
        assert_eq!(name, "OnModuleAttributeChanges");
        let [Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
        for (_, attr, old, new) in expected.iter().filter(|change| change.0 == Key::Ship) {
            assert!(rows.iter().any(|row| {
                let Value::Tuple(fields) = row else { return false };
                fields[3].as_int() == Some(i64::from(*attr))
                    && fields[5] == Value::Float(*new) && fields[6] == Value::Float(*old)
            }));
        }
    }
}

#[test]
fn publication_preserves_intervening_active_projection_and_damage() {
    use crate::services::fit::attributes::tests::module_fit;
    crate::tests::init_config();
    let sde = crate::sde::types::tests::combat_fixture();
    let before = module_fit(Arc::clone(&sde), 1, 1, true);
    let final_fit = module_fit(Arc::clone(&sde), 1, 1, true);
    let patch = final_fit.into_committed_fitting(&before);
    let mut current = module_fit(sde, 1, 1, true);
    current.set_module_active(2, true);
    current.set_module_overloaded(2, true);
    current.replace_projections_for(Key::Ship, vec![], Some(9));
    let registry = Arc::new(crate::space::Pilots::default());
    let mut system = System::new(1, registry, 0);
    system.add_persistent_ship(hull());
    let generation = system.persistent_generation(1).unwrap();
    let stats = ShipStats::from_fit(&current, Key::Ship);
    system.install_condition(1, Condition { now: [50.0, 60.0, 70.0], stats,
        shield_at: crate::client::filetime_now() + infinity_core::consts::FILETIME_SECOND }, current);
    assert!(system.publish_fitting(1, 2, generation, patch, vec![]));
    let record = &system.actor_combat[&1];
    assert_eq!(record.fit.module_state(2), Some((true, true)));
    assert_eq!(record.fit.projection_revision(), Some(9));
    assert_eq!(record.condition.now, [50.0, 60.0, 70.0]);
}

#[test]
fn shield_rate_changes_publish_one_authoritative_charge() {
    use crate::services::fit::attributes::tests::ship_fit;
    crate::tests::init_config();
    for (ship, shield, max_change) in [(-920_101, 100.0, false), (-920_102, 40.0, false),
        (-920_103, 100.0, true), (-920_104, 40.0, true)] {
        let before = ship_fit(&[]);
        let mut attrs = vec![(ATTR_SHIELD_RECHARGE_RATE, 2000.0)];
        if max_change { attrs.push((ATTR_SHIELD_CAPACITY, 60.0)); }
        let patch = ship_fit(&attrs).into_committed_fitting(&before);
        let registry = Arc::new(crate::space::Pilots::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        registry.register_pilot(2, tx);
        let mut system = System::new(1, registry, 0);
        let mut ball = hull();
        ball.id = ship;
        system.add_persistent_ship(ball);
        let generation = system.persistent_generation(ship).unwrap();
        let stats = ShipStats::from_fit(&before, Key::Ship);
        system.install_condition(ship, Condition { now: [shield, 100.0, 100.0], stats,
            shield_at: crate::client::filetime_now() + infinity_core::consts::FILETIME_SECOND }, before);
        assert!(system.publish_fitting(ship, 2, generation, patch, vec![]));
        let (_, args) = rx.try_recv().unwrap();
        let [Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
        let charges: Vec<_> = rows.iter().filter_map(|row| {
            let Value::Tuple(fields) = row else { return None };
            (fields[3].as_int() == Some(i64::from(ATTR_SHIELD_CHARGE))).then_some(fields[5].clone())
        }).collect();
        assert_eq!(charges, vec![Value::Float(if max_change { shield.min(60.0) } else { shield })]);
    }
}

#[test]
fn fitting_motion_applies_at_destiny_boundary_and_rejects_stale_incarnation() {
    use infinity_core::consts::dogma::{ATTR_AGILITY, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_SIGNATURE_RADIUS};
    use crate::services::fit::attributes::tests::ship_fit;
    crate::tests::init_config();
    for stale in 0..3 {
        let before = ship_fit(&[]);
        let patch = ship_fit(&[(ATTR_MASS, 42.0), (ATTR_MAX_VELOCITY, 123.0),
            (ATTR_AGILITY, 2.0), (ATTR_SIGNATURE_RADIUS, 44.0)]).into_committed_fitting(&before);
        let mut system = System::new(1, Arc::default(), 0);
        system.add_persistent_ship(hull());
        let generation = system.persistent_generation(1).unwrap();
        let stats = ShipStats::from_fit(&before, Key::Ship);
        system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, before);
        assert!(system.publish_fitting(1, 2, generation, patch, vec![]));
        assert_eq!(system.ball(1).unwrap().mass, 1.0);
        if stale == 1 { system.actor_combat.get_mut(&1).unwrap().phase = CombatPhase::Transition { token: 1 }; }
        if stale == 2 {
            system.remove_ball(1);
            system.add_persistent_ship(hull());
            let fit = ship_fit(&[]);
            let stats = ShipStats::from_fit(&fit, Key::Ship);
            system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, fit);
            assert_ne!(system.persistent_generation(1), Some(generation));
        }
        system.tick(1000);
        let ball = system.ball(1).unwrap();
        if stale != 0 { assert_eq!(ball.mass, 1.0); } else {
            assert_eq!((ball.mass, ball.max_vel, ball.agility, ball.signature_radius), (42.0, 123.0, 2.0, Some(44.0)));
        }
    }
}

#[test]
fn offline_patch_clears_live_overload_and_publishes_effective_values() {
    use crate::services::fit::attributes::tests::module_fit;
    crate::tests::init_config();
    let sde = crate::sde::types::tests::overload_fixture();
    let before = module_fit(Arc::clone(&sde), 1, 2, true);
    let patch = module_fit(Arc::clone(&sde), 1, 2, false).into_committed_fitting(&before);
    let mut current = module_fit(sde, 1, 2, true);
    current.set_module_active(2, true);
    current.set_module_overloaded(2, true);
    assert_eq!(current.value(Key::Ship, ATTR_ARMOR_HP), 150.0);
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new(1, registry, 0);
    system.add_persistent_ship(hull());
    let generation = system.persistent_generation(1).unwrap();
    let stats = ShipStats::from_fit(&current, Key::Ship);
    system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, current);
    assert!(system.publish_fitting(1, 2, generation, patch, vec![]));
    let fit = &system.actor_combat[&1].fit;
    assert_eq!(fit.module_state(2), Some((false, false)));
    assert_eq!(fit.value(Key::Ship, ATTR_ARMOR_HP), 100.0);
    let (_, args) = rx.try_recv().unwrap();
    let [Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
    assert!(rows.iter().any(|row| {
        let Value::Tuple(fields) = row else { return false };
        fields[3].as_int() == Some(i64::from(ATTR_ARMOR_HP))
            && fields[5] == Value::Float(100.0) && fields[6] == Value::Float(150.0)
    }));
}

fn rig() -> (Arc<crate::sde::Sde>, Arc<crate::space::Pilots>) {
    crate::tests::init_config();
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    (crate::sde::types::tests::combat_fixture(), registry)
}

fn install_fit(system: &mut System, ship: i64, fit: Fit) -> u64 {
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(ship, Condition { now: stats.max, stats, shield_at: 0 }, fit);
    system.combat_membership_generation(ship).unwrap()
}

fn install_drone(system: &mut System, type_id: i32, fit: Fit) {
    use crate::space::drone::{DroneEffects, DroneSpec};
    system.insert(Ball { id: 5, type_id, char_id: None, owner_id: 2, ..hull() });
    let spec = DroneSpec { type_id, bandwidth: 5.0, scan_resolution: 100.0, control_range: 1000.0,
        max_target_range: 1000.0, optimal_range: 100.0, orbit_range: 50.0, duration_ms: 1000.0, effect_id: Some(1) };
    let effects: DroneEffects = Arc::new(|_, _| None);
    system.register_drone(5, spec, effects, Some(1), Some(2), false);
    let stats = ShipStats::from_fit(&fit, Key::Drone(5));
    system.install_condition_with_key(5, Condition { now: stats.max, stats, shield_at: 0 }, fit, Key::Drone(5));
}

fn character(at: i64, expiry: Option<i64>) -> crate::services::fit::character::CharacterInputs {
    use crate::dao::{items::brain::BrainItem, skills::training::Skill};
    use crate::services::fit::character::BrainInput;
    use infinity_core::consts::{FLAG_BOOSTER, FLAG_IMPLANT};
    let booster = expiry.map(|expires_at| BrainInput { overrides: vec![], item: BrainItem { item_id: 5,
        type_id: 3, flag_id: FLAG_BOOSTER, slot: None, activated_at: Some(at), expires_at: Some(expires_at), effect_ids: Some(vec![3]) } });
    crate::services::fit::character::CharacterInputs {
        at, attributes: Some(vec![(1, 1.0)]), trained: vec![Skill { type_id: 7, level: 5, skill_points: 8000 }],
        installations: vec![],
        brain: booster.into_iter().chain([BrainInput { overrides: vec![], item: BrainItem { item_id: 6, type_id: 4,
            flag_id: FLAG_IMPLANT, slot: None, activated_at: None, expires_at: None, effect_ids: None } }]).collect(),
    }
}

#[test]
fn stale_gates_reject_and_due_expiry_fires_before_its_timer() {
    let (sde, registry) = rig();
    let mut system = System::new(1, registry, 0);
    system.add_persistent_ship(hull());
    system.insert(Ball { id: 8, char_id: None, ..hull() });
    let generation = system.persistent_generation(1).unwrap();
    install_fit(&mut system, 1, Fit::for_space_type(sde.clone(), 1, vec![]));
    let inputs = character(1, None);
    assert!(!system.publish_character_inputs(1, 2, generation.wrapping_add(1), &inputs));
    assert!(!system.publish_character_inputs(1, 3, generation, &inputs));
    assert!(system.publish_character_inputs(1, 2, generation, &inputs));
    system.snapshot_positions.insert(1, (generation.wrapping_add(1), [0.0; 3]));
    assert!(!system.publish_character_inputs(1, 2, generation, &inputs));
    assert!(system.with_combat_fit(1, |_, _, _| ()).is_none());
    system.snapshot_positions.insert(1, (generation, [0.0; 3]));
    system.actor_combat.get_mut(&1).unwrap().phase = CombatPhase::Transition { token: 1 };
    assert!(!system.publish_character_inputs(1, 2, generation, &inputs));
    system.actor_combat.get_mut(&1).unwrap().phase = CombatPhase::Live;
    assert_eq!(system.with_combat_fit(1, |fit, _, stamp| (fit.source_type(Key::Skill(7)), stamp.membership)),
        Some((Some(7), generation)));
    // A changeless read does not grow the fit revision.
    let revision = system.combat_fit_revision(1);
    assert!(system.with_combat_fit(1, |_, _, _| ()).is_some());
    assert_eq!(system.combat_fit_revision(1), revision);
    // An elapsed deadline stays armed until the first fit touch; even a pure
    // projection renewal passes the shared gate and consumes it.
    use crate::services::effects::{projection::Projection, EffectKind, JammingType};
    let projection = |until_ms| Projection { source_ship: 8, source_module: 9, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms,
        values: vec![(1, -1.0)], charge_values: vec![], charge_type: None, scramble: None };
    system.upsert_projection(1, projection(100));
    let now = crate::client::filetime_now();
    let expiry = now - infinity_core::consts::FILETIME_SECOND;
    let mut fit = crate::services::fit::attributes::tests::module_fit(sde.clone(), 1, 1, true);
    fit.apply_character_inputs(&character(now - 2 * infinity_core::consts::FILETIME_SECOND, Some(expiry)));
    install_fit(&mut system, 1, fit);
    assert!(system.actor_combat[&1].character_timer.is_some_and(|(deadline, _)| deadline == expiry));
    system.upsert_projection(1, projection(i64::MAX));
    assert_eq!(system.with_combat_fit(1, |fit, _, _| fit.source_type(Key::Booster(5))), Some(None));
    assert!(system.actor_combat[&1].character_timer.is_none());
    let revision = system.combat_fit_revision(1).unwrap();
    assert!(revision > 0);
    assert_eq!(system.with_combat_fit(1, |fit, _, _| fit.source_type(Key::Implant(6))), Some(Some(4)));
    assert_eq!(system.combat_fit_revision(1), Some(revision));
    // A no-op flag delta still rebases a drone whose own expiry the shared
    // prepare consumed: the queued kinematics write is the output fact.
    let mut drone_fit = crate::services::fit::attributes::tests::module_fit(sde.clone(), 1, 1, false);
    drone_fit.apply_character_inputs(&character(now - 2 * infinity_core::consts::FILETIME_SECOND, Some(expiry)));
    install_drone(&mut system, 1, drone_fit);
    assert!(system.ball(5).unwrap().signature_radius.is_none());
    assert!(system.fitted_module_online(1, 2, false));
    system.tick(1000);
    assert!(system.ball(5).unwrap().signature_radius.is_some());
    assert_eq!(system.with_combat_fit(5, |fit, _, _| fit.source_type(Key::Booster(5))), Some(None));
}

#[test]
fn publish_replaces_domain_on_ship_and_owned_drones_and_preserves_live_state() {
    use crate::services::effects::{projection::Projection, EffectKind, JammingType};
    let (sde, registry) = rig();
    let mut system = System::new(1, registry, 0);
    system.add_persistent_ship(hull());
    system.insert(Ball { id: 8, char_id: None, ..hull() });
    let generation = system.persistent_generation(1).unwrap();
    let now = crate::client::filetime_now();
    let mut fit = crate::services::fit::attributes::tests::module_fit(sde.clone(), 1, 1, true);
    fit.apply_character_inputs(&character(now, None));
    install_fit(&mut system, 1, fit);
    let mut drone_fit = crate::services::fit::attributes::tests::module_fit(sde.clone(), 1, 1, true);
    drone_fit.include_drone(5, 1, vec![]);
    drone_fit.apply_character_inputs(&character(now, None));
    install_drone(&mut system, 1, drone_fit);
    assert!(system.set_condition_fraction(1, 0.5));
    let (_, seeded) = crate::services::combat::groups_health::fill_capacitor(1, 100.0, 1000.0, crate::client::filetime_now());
    system.upsert_projection(1, Projection { source_ship: 8, source_module: 9, source_type: 1,
        effect_id: 1, kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
        values: vec![(1, -10.0)], charge_values: vec![], charge_type: None, scramble: None });
    assert!(system.publish_character_inputs(1, 2, generation,
        &crate::services::fit::character::CharacterInputs { at: now + 1, trained: vec![], ..character(now, None) }));
    assert_eq!(system.with_combat_fit(1, |fit, _, _| (
        fit.source_type(Key::Skill(7)).is_none(), fit.source_type(Key::Projected(9)).is_some())), Some((true, true)));
    assert_eq!(system.with_combat_fit(5, |fit, _, _| fit.source_type(Key::Skill(7)).is_none()), Some(true));
    assert!(system.ship_condition(1).unwrap().1 > 0.0);
    let (cap, _) = crate::services::combat::groups_health::rebase_capacitor(1, 100.0, 1000.0, 100.0, 1000.0);
    assert_eq!(cap, seeded);
    assert_eq!(system.drone(5).unwrap().owner, 2);
    // Runtime flags and committed ship-domain deltas propagate into every
    // controlled drone's resident copy; its own domain is untouched.
    assert!(system.fitted_module_online(1, 2, false));
    assert_eq!(system.with_combat_fit(5, |fit, _, _| fit.module_state(2)), Some(Some((false, false))));
    let patch = Fit::for_space_type(sde.clone(), 1, vec![])
        .into_committed_fitting(&crate::services::fit::attributes::tests::module_fit(sde.clone(), 1, 1, false));
    assert!(system.publish_fitting(1, 2, generation, patch, vec![]));
    assert_eq!(system.with_combat_fit(5, |fit, _, _| fit.module_state(2).is_none()), Some(true));
    // Real owner packages and override deltas pass only their own gates:
    // wrong owner, stale drone generation and stale membership are refused.
    assert!(system.drone_order(5, 2, 1, crate::space::drone::DroneOrder::Assist(3)));
    let drone_generation = system.drone(5).unwrap().generation;
    let drone_membership = system.combat_membership_generation(5).unwrap();
    let mut source = crate::services::fit::attributes::tests::owner_fit(sde.clone(), 1);
    source.apply_character_inputs(&character(1, Some(2)));
    let owner_inputs = source.owner_inputs().unwrap();
    assert!(!system.drone_publication_current(5, 3, drone_generation, drone_membership));
    assert!(!system.drone_publication_current(5, 2, drone_generation.wrapping_add(1), drone_membership));
    assert!(!system.drone_publication_current(5, 2, drone_generation, drone_membership.wrapping_add(1)));
    system.actor_combat.get_mut(&5).unwrap().phase = CombatPhase::Transition { token: 1 };
    assert!(!system.drone_publication_current(5, 2, drone_generation, drone_membership));
    system.actor_combat.get_mut(&5).unwrap().phase = CombatPhase::Live;
    system.actor_combat.get_mut(&5).unwrap().fit.strip_to_own_drone(5);
    assert!(system.actor_combat[&5].fit.source_type(Key::Char).is_none());
    assert!(system.actor_combat[&5].fit.next_character_expiry().is_none());
    assert!(system.publish_drone_owner_inputs(5, 2, drone_generation, drone_membership, &owner_inputs));
    assert!(system.actor_combat[&5].fit.source_type(Key::Booster(5)).is_none());
    assert!(system.actor_combat[&5].fit.source_type(Key::Implant(6)).is_some());
    assert!(system.actor_combat[&5].character_timer.is_none());
    assert_eq!(system.with_combat_fit(5, |fit, _, _| fit.source_type(Key::Ship)), Some(Some(1)));
    let patch = |attr, value| CommittedFitting::overridden_attributes(vec![(Key::Drone(5), attr, value)]);
    assert!(system.publish_drone_fitting(5, 2, drone_generation, drone_membership, patch(ATTR_ARMOR_HP, Some(50.0))));
    assert_eq!(system.with_combat_fit(5, |fit, _, _| fit.override_state(Key::Drone(5), ATTR_ARMOR_HP).map(|s| s.value)),
        Some(Some(50.0)));
    let current = system.drone(5).unwrap();
    assert_eq!((current.owner, current.controller, current.controller_owner), (2, Some(1), Some(2)));
    assert_eq!(current.generation, drone_generation);
    assert_eq!(current.order, crate::space::drone::DroneOrder::Assist(3));
    // Abandon detaches the owner domain; the drone keeps its own record and
    // condition and gains no new rights from losing its controller.
    system.drone_abandoned(5);
    assert_eq!(system.drone(5).unwrap().owner, infinity_core::consts::OWNER_UNOWNED);
    let abandoned = system.drone(5).unwrap();
    assert!(!system.drone_publication_current(5, abandoned.owner, abandoned.generation, drone_membership));
    let stripped = system.with_combat_fit(5, |fit, _, _| (
        fit.source_type(Key::Char).is_none(), fit.source_type(Key::Ship).is_none(), fit.source_type(Key::Skill(7)).is_none()));
    assert_eq!(stripped, Some((true, true, true)));
    assert!(system.combat_condition_snapshot(5).is_some());
}

#[test]
fn overloaded_toggle_publishes_paired_values_and_noop_stays_silent() {
    use crate::services::fit::attributes::tests::module_fit;
    crate::tests::init_config();
    for piloted in [true, false] {
        let registry = Arc::new(crate::space::Pilots::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        if piloted { registry.register_pilot(2, tx); }
        let mut system = System::new(1, registry, 0);
        system.add_persistent_ship(hull());
        install_fit(&mut system, 1, module_fit(crate::sde::types::tests::overload_fixture(), 1, 2, true));
        assert!(system.fitted_module_overloaded(1, 2, true));
        assert_eq!(system.with_combat_fit(1, |fit, _, _| fit.value(Key::Ship, ATTR_ARMOR_HP)), Some(150.0));
        if piloted {
            let batches = attr_batches(&mut rx);
            let hp = batches[0].iter().position(|r| *r == (2, 1, ATTR_ARMOR_HP, 150.0, 100.0)).unwrap();
            assert_eq!(batches[0][hp + 1], (2, 1, ATTR_ARMOR_DAMAGE, 0.0, 0.0));
        }
        assert!(system.fitted_module_overloaded(1, 2, false));
        assert!(system.fitted_module_overloaded(1, 2, false), "a same-state write is admitted");
        if piloted { assert_batch(&mut rx, 1, 2, Key::Ship, &[(Key::Ship, ATTR_ARMOR_HP, 150.0, 100.0)]); }
        else { assert!(rx.try_recv().is_err()); }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn character_publications_cover_deadlines_and_merged_mutations() {
    use crate::{dao::skills::expert_systems::ExpertSystem, services::fit::character::CharacterInputs};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hulls: Vec<i32> = sde.types.iter().filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && ShipStats::from_fit(&Fit::for_space_type(Arc::clone(&sde), t.type_id, vec![]), Key::Ship)
            .max.iter().all(|v| v.is_finite() && *v > 0.0)).map(|t| t.type_id).collect();
    let now = crate::client::filetime_now();
    let sec = infinity_core::consts::FILETIME_SECOND;
    // Before/after fits for character inputs, optionally past their deadline.
    let pair = |inputs: &CharacterInputs, hull: i32, expire: bool| {
        let (mut before, mut after) = (Fit::for_space_type(Arc::clone(&sde), hull, vec![]),
            Fit::for_space_type(Arc::clone(&sde), hull, vec![]));
        before.apply_character_inputs(inputs);
        after.apply_character_inputs(inputs);
        if expire { after.expire_character_inputs(now); }
        (before, after)
    };
    let changes = |before: &Fit, after: &Fit|
        after.fitting_changes(&before.fitting_values(&before.fitting_candidates(after)));
    let install = |type_id, at: i64, installed_at: i64, expires_at: i64| CharacterInputs { at,
        installations: vec![ExpertSystem { character_id: 2, type_id, installed_at, expires_at }],
        ..Default::default() };
    // One real expert grant that moves ship attributes while active; an
    // incoming-only receiver pair must exist.
    let mut found = None;
    'grants: for def in sde.expert_systems.iter() {
        for &hull in def.associated_ship_types.iter().chain(hulls.iter().take(64)) {
            let before = Fit::for_space_type(Arc::clone(&sde), hull, vec![]);
            let mut after = Fit::for_space_type(Arc::clone(&sde), hull, vec![]);
            after.apply_character_inputs(&install(def.type_id, now - sec, now - sec, now + 4 * sec));
            let delta = changes(&before, &after);
            if !delta.iter().any(|(key, _, _, _)| *key == Key::Ship) { continue; }
            let incoming = delta.iter()
                .any(|c| !before.fitting_candidates(&before).contains(&(c.0, c.1)));
            if found.is_none() || incoming { found = Some((def.type_id, hull, delta, incoming)); }
            if incoming { break 'grants; }
        }
    }
    let (grant, hull_type, publish_changes, incoming) = found.expect("SDE needs a ship-modifying expert grant");
    assert!(incoming, "an incoming-only receiver attribute is required");
    let publish = install(grant, now, now - sec, now + 4 * sec);
    let activate = install(grant, now - 4 * sec, now - sec, now + 4 * sec);
    let expire = install(grant, now - 4 * sec, now - 4 * sec, now - sec);
    let registry = Arc::new(crate::space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(2, tx);
    let mut system = System::new(1, registry, 0);
    let mut ball = hull(); ball.type_id = hull_type;
    system.add_persistent_ship(ball);
    let generation = system.persistent_generation(1).unwrap();
    install_fit(&mut system, 1, Fit::for_space_type(Arc::clone(&sde), hull_type, vec![]));
    // A wrong owner and a stale generation are refused before any notification.
    assert!(!system.publish_character_inputs(1, 3, generation, &publish));
    assert!(!system.publish_character_inputs(1, 2, generation.wrapping_add(1), &publish));
    assert!(attr_batches(&mut rx).is_empty());
    assert!(system.publish_character_inputs(1, 2, generation, &publish));
    assert_batch(&mut rx, 1, 2, Key::Ship, &publish_changes);
    // Removing the domain restores the base rows in one batch.
    assert!(system.publish_character_inputs(1, 2, generation, &CharacterInputs::default()));
    let (with_grant, _) = pair(&publish, hull_type, false);
    let bare = Fit::for_space_type(Arc::clone(&sde), hull_type, vec![]);
    assert_batch(&mut rx, 1, 2, Key::Ship, &bare.fitting_changes(
        &with_grant.fitting_values(&with_grant.fitting_candidates(&bare))));
    // Due activation/expiry land once via the first read or the armed timer.
    for (timer, inputs) in [(false, &activate), (false, &expire), (true, &expire)] {
        let (before, after) = pair(inputs, hull_type, true);
        let expected = changes(&before, &after);
        assert!(!expected.is_empty(), "deadline inputs must change ship attributes");
        install_fit(&mut system, 1, before);
        if timer { system.tick(crate::space::now_ms()); }
        else { assert!(system.with_combat_fit(1, |_, _, _| ()).is_some()); }
        assert_batch(&mut rx, 1, 2, Key::Ship, &expected);
    }
    // A due expiry and a committed domain removal merge into a single batch.
    let before = pair(&expire, hull_type, false).0;
    let expected = changes(&before, &bare);
    install_fit(&mut system, 1, before);
    assert!(system.publish_character_inputs(1, 2, generation, &CharacterInputs::default()));
    assert_batch(&mut rx, 1, 2, Key::Ship, &expected);
    // A due expiry merges into a no-op module write's one old->final batch.
    let module_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)).unwrap().type_id;
    let module = |due: bool| { let mut fit = crate::services::fit::attributes::tests::module_fit(
        Arc::clone(&sde), hull_type, module_type, true);
        if due { fit.apply_character_inputs(&expire); } fit };
    let mutations: [fn(&mut System) -> bool; 3] = [
        |s| s.fitted_module_online(1, 2, true), |s| s.fitted_module_active(1, 2, false),
        |s| s.fitted_module_overloaded(1, 2, false)];
    for mutate in mutations {
        install_fit(&mut system, 1, module(true));
        mutate(&mut system);
        assert_batch(&mut rx, 1, 2, Key::Ship, &changes(&module(true), &module(false)));
        assert!(system.with_combat_fit(1, |_, _, _| ()).is_some());
        assert!(attr_batches(&mut rx).is_empty());
    }
}
