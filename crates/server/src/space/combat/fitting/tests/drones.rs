//! Owned-drone own-key publications against the real SDE: committed own
//! override maxima pair with their current levels, a same-owner hull handoff
//! publishes its own delta, and abandon emits final attributes before state
//! and activity. Level-5 points reuse the client formula locally because
//! `services::skills::formulas::sp_for_level` is service-private.
use super::*;
use crate::dao::skills::training::Skill;
use crate::services::fit::{attributes::tests::owner_fit, character::OwnerInputs};
use crate::space::tests::fit_delta::{pilot_channels, NotifyRx};
use infinity_core::consts::{CATEGORY_DRONE, SKILL_DRONE_DURABILITY,
    dogma::{ATTR_REQUIRED_SKILLS, ATTR_SKILL_TIME_CONSTANT}};

/// Fresh actor holding a resident `Hobgoblin I` already published the
/// selected-hull package, with the wire snapshot and fences needed to assert it.
async fn drone_fixture() -> (System, Fit, Vec<(Key, i32, f64)>, OwnerInputs, u64, u64, NotifyRx, NotifyRx) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let drones = sde.types.iter().find(|t| t.name.as_deref() == Some("Drones")).expect("named Drones skill").type_id;
    let drone = sde.types.iter().find(|t| t.name.as_deref() == Some("Hobgoblin I")
        && t.category_id == Some(CATEGORY_DRONE)).expect("authored Hobgoblin I");
    assert!(ATTR_REQUIRED_SKILLS.iter().any(|a| drone.attribute(*a) == Some(f64::from(drones))));
    let rank = sde.types.get(SKILL_DRONE_DURABILITY).and_then(|t| t.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).expect("rank");
    let skill = Skill { type_id: SKILL_DRONE_DURABILITY, level: 5, skill_points: (rank * 250.0 * 1024.0) as i64 };
    let now = crate::client::filetime_now();
    let build = |ship, inputs: &CharacterInputs| {
        let mut fit = owner_fit(Arc::clone(&sde), ship);
        fit.include_drone(5, drone.type_id, vec![(ATTR_CAPACITOR_CAPACITY, 100.0),
            (ATTR_RECHARGE_RATE, f64::MAX), (ATTR_SHIELD_RECHARGE_RATE, f64::MAX)]);
        fit.apply_character_inputs(inputs);
        fit
    };
    let resident = build(11, &CharacterInputs { at: now, ..Default::default() });
    assert_eq!(resident.owner_inputs().unwrap().ship, 11);
    let oracle = build(12, &CharacterInputs { at: now, trained: vec![skill], ..Default::default() });
    let inputs = oracle.owner_inputs().unwrap();
    assert_eq!(inputs.ship, 12);
    let before = resident.fitting_values(&resident.fitting_candidates(&oracle));
    let (registry, owner_rx, delegate_rx) = pilot_channels();
    let mut system = System::new(1, registry, 0);
    install_drone(&mut system, drone.type_id, resident);
    crate::services::combat::groups_health::fill_capacitor(5, 100.0, f64::MAX, now);
    let generation = system.drone(5).unwrap().generation;
    let membership = system.combat_membership_generation(5).unwrap();
    assert!(system.publish_drone_owner_inputs(5, 2, generation, membership, &inputs));
    (system, oracle, before, inputs, generation, membership, owner_rx, delegate_rx)
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn own_drone_maxima_pair_current_levels() {
    let (mut system, mut oracle, .., mut owner_rx, mut delegate_rx) = drone_fixture().await;
    assert_eq!(attr_batches(&mut owner_rx).len(), 1);
    assert!(system.set_condition_fraction(5, 0.25));
    let (s0, a0, h0) = system.ship_condition(5).unwrap();
    let [shield, armor, hull] = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].map(|a| system.actor_combat[&5].fit.value(Key::Drone(5), a));
    assert!([shield, armor, hull].iter().all(|v| v.is_finite() && *v > 0.0) && 0.0 == system.actor_combat[&5].fit.value(Key::Ship, ATTR_SHIELD_CAPACITY));
    let patch = CommittedFitting::overridden_attributes(vec![(Key::Drone(5), ATTR_STRUCTURE_HP, Some(hull * 2.0)),
        (Key::Drone(5), ATTR_SHIELD_CAPACITY, Some(shield / 8.0)), (Key::Drone(5), ATTR_ARMOR_HP, Some(armor * 2.0)),
        (Key::Drone(5), ATTR_CAPACITOR_CAPACITY, Some(200.0))]);
    assert!(oracle.apply_committed_fitting(&patch));
    let before = system.actor_combat[&5].fit.fitting_values(&system.actor_combat[&5].fit.fitting_candidates(&oracle));
    assert!(system.publish_drone_fitting(5, 2, system.drone(5).unwrap().generation,
        system.combat_membership_generation(5).unwrap(), patch));
    let batch = assert_batch(&mut owner_rx, 5, 2, Key::Drone(5), &oracle.fitting_changes(&before));
    assert_eq!(batch, vec![(2, 5, ATTR_STRUCTURE_HP, oracle.value(Key::Drone(5), ATTR_STRUCTURE_HP), hull), (2, 5, ATTR_HULL_DAMAGE, h0, h0),
        (2, 5, ATTR_SHIELD_CAPACITY, oracle.value(Key::Drone(5), ATTR_SHIELD_CAPACITY), shield), (2, 5, ATTR_SHIELD_CHARGE, s0.min(oracle.value(Key::Drone(5), ATTR_SHIELD_CAPACITY)), s0),
        (2, 5, ATTR_ARMOR_HP, oracle.value(Key::Drone(5), ATTR_ARMOR_HP), armor), (2, 5, ATTR_ARMOR_DAMAGE, a0, a0),
        (2, 5, ATTR_CAPACITOR_CAPACITY, oracle.value(Key::Drone(5), ATTR_CAPACITOR_CAPACITY), 100.0), (2, 5, ATTR_CHARGE, 100.0, 100.0)]);
    assert!(attr_batches(&mut delegate_rx).is_empty());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn same_owner_selected_hull_handoff_publishes_own_delta() {
    let (system, oracle, before, inputs, generation, membership, mut owner_rx, mut delegate_rx) =
        drone_fixture().await;
    let expected: Vec<_> = oracle.fitting_changes(&before).into_iter()
        .filter(|r| r.0 == Key::Drone(5)).collect();
    assert!(!expected.is_empty() && inputs.ship == 12);
    assert_batch(&mut owner_rx, 5, 2, Key::Drone(5), &expected);
    assert!(attr_batches(&mut delegate_rx).is_empty());
    let drone = system.drone(5).unwrap();
    assert_eq!((drone.owner, drone.generation, drone.controller, drone.controller_owner),
        (2, generation, Some(1), Some(2)));
    assert_eq!(system.combat_membership_generation(5), Some(membership));
    assert_eq!(system.actor_combat[&5].fit.owner_inputs().unwrap().ship, 12);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn abandon_publishes_final_attributes_before_state_and_activity() {
    let (mut system, mut oracle, .., mut owner_rx, mut delegate_rx) = drone_fixture().await;
    assert_eq!(attr_batches(&mut owner_rx).len(), 1);
    oracle.strip_to_own_drone(5);
    let fit = &system.actor_combat[&5].fit;
    let expected: Vec<_> = oracle.fitting_changes(&fit.fitting_values(&fit.fitting_candidates(&oracle)))
        .into_iter().filter(|r| r.0 == Key::Drone(5)).collect();
    assert!(!expected.is_empty());
    system.drone_abandoned(5);
    let mut events = Vec::new();
    while let Ok(event) = owner_rx.try_recv() {
        events.push(event);
    }
    let position = |name| events.iter().position(|event| event.0 == name).unwrap();
    let (attr, state, activity) = (position("OnModuleAttributeChanges"),
        position("OnDroneStateChange"), position("OnDroneActivityChange"));
    assert!(attr < state && state < activity);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(events.swap_remove(attr)).unwrap();
    assert_batch(&mut rx, 5, 2, Key::Drone(5), &expected);
    assert!(attr_batches(&mut delegate_rx).is_empty());
    system.drone_abandoned(5);
    assert!(attr_batches(&mut owner_rx).is_empty());
}
