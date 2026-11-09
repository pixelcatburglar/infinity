//! Actor fitting publication fences against stale pilots and incarnations.
use super::*;
use crate::services::combat::groups_health::{Condition, ShipStats};
use crate::services::fit::Fit;
use crate::space::{Ball, MODE_STOP};
use std::sync::Arc;

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
