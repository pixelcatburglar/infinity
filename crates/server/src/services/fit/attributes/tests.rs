//! Database-free regression tests for committed fitting field merges.

use super::*;
#[cfg(feature = "tests-effects")] use infinity_core::consts::dogma::{ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP};
#[cfg(any(feature = "tests-effects", feature = "tests-space"))] use crate::services::fit::Location;

#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-space"))]
fn fit() -> Fit {
    Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, vec![])
}

#[cfg(feature = "tests-effects")]
fn module(fit: &mut Fit, id: i64, online: bool, overrides: Vec<(i32, f64)>) {
    fit.nodes.insert(Key::Module(id), Node {
        type_id: 1, data_position: fit.sde.types.position(1), location: Location::Ship,
        online, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides, cargo_unlimited: false, charge_values: vec![],
    });
}

#[cfg(any(feature = "tests-combat", feature = "tests-ships", feature = "tests-space"))]
/// Build a ship Fit with explicit synthetic attribute overrides.
pub(crate) fn ship_fit(overrides: &[(i32, f64)]) -> Fit {
    let mut fit = fit();
    let node = fit.nodes.get_mut(&Key::Ship).unwrap();
    node.overrides = overrides.to_vec();
    node.overrides.sort_unstable_by_key(|entry| entry.0);
    fit
}

#[cfg(any(feature = "tests-effects", feature = "tests-space"))]
/// Build a synthetic fitted module using real static effect data when supplied.
pub(crate) fn module_fit(sde: std::sync::Arc<crate::sde::Sde>, hull: i32, module_type: i32, online: bool) -> Fit {
    let mut fit = Fit::for_space_type(sde, hull, vec![]);
    fit.nodes.insert(Key::Module(2), Node {
        type_id: module_type, data_position: fit.sde.types.position(module_type), location: Location::Ship,
        online, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides: vec![], cargo_unlimited: false, charge_values: vec![],
    });
    fit
}

#[cfg(feature = "tests-space")]
/// A ship-domain module fit that owns a real ship item, for owner-package tests.
pub(crate) fn owner_fit(sde: std::sync::Arc<crate::sde::Sde>, ship: i64) -> Fit {
    let mut fit = module_fit(sde, 1, 1, true);
    fit.ship_id = Some(ship);
    fit
}

#[cfg(feature = "tests-effects")] #[test]
fn field_delta_preserves_runtime_and_unmodified_damage() {
    let mut before = fit();
    let mut current = fit();
    let mut final_fit = fit();
    for f in [&mut before, &mut current, &mut final_fit] {
        module(f, 2, true, vec![(ATTR_HULL_DAMAGE, 1.0), (ATTR_STRUCTURE_HP, 200.0)]);
        f.ship_id = Some(9);
        f.nodes.get_mut(&Key::Ship).unwrap().overrides.push((infinity_core::consts::dogma::ATTR_CAPACITY, 400.0));
        f.nodes.get_mut(&Key::Ship).unwrap().overrides.sort_unstable_by_key(|entry| entry.0);
    }
    current.nodes.get_mut(&Key::Module(2)).unwrap().overrides[0].1 = 8.0;
    current.set_module_active(2, true);
    current.set_module_overloaded(2, true);
    current.projection_revision = Some(7);
    final_fit.nodes.get_mut(&Key::Module(2)).unwrap().overrides.pop();
    // The capacity-mode toggle rides the same committed patch; it publishes
    // the wire capacity without touching damage, runtime flags or projection.
    final_fit.nodes.get_mut(&Key::Ship).unwrap().cargo_unlimited = true;
    let patch = final_fit.into_committed_fitting(&before);
    assert!(patch.candidates().iter().any(|(_, a)| *a == infinity_core::consts::dogma::ATTR_CAPACITY));
    let old = current.fitting_values(patch.candidates());
    current.apply_committed_fitting(&patch);
    let node = &current.nodes[&Key::Module(2)];
    assert_eq!(node.overrides, vec![(ATTR_HULL_DAMAGE, 8.0)]);
    assert!(node.active && node.overloaded);
    assert_eq!(current.projection_revision, Some(7));
    assert!(current.fitting_changes(&old).contains(&(Key::Module(2), ATTR_STRUCTURE_HP, 200.0, 100.0)));
    assert!(current.nodes[&Key::Ship].cargo_unlimited);
    assert!(current.cargo_unlimited());
    assert_eq!(current.wire_value(Key::Ship, infinity_core::consts::dogma::ATTR_CAPACITY),
        crate::services::fit::cargo::UNLIMITED_CARGO_WIRE_CAPACITY);
    // Clearing through a real cold delta restores the finite value and still
    // keeps the module's runtime state untouched.
    before.nodes.get_mut(&Key::Ship).unwrap().cargo_unlimited = true;
    let mut final_fit = fit();
    final_fit.ship_id = Some(9);
    final_fit.nodes.get_mut(&Key::Ship).unwrap().overrides.push((infinity_core::consts::dogma::ATTR_CAPACITY, 400.0));
    module(&mut final_fit, 2, true, vec![(ATTR_HULL_DAMAGE, 1.0), (ATTR_STRUCTURE_HP, 200.0)]);
    current.apply_committed_fitting(&final_fit.into_committed_fitting(&before));
    let node = &current.nodes[&Key::Module(2)];
    assert_eq!(node.overrides, vec![(ATTR_HULL_DAMAGE, 8.0)]);
    assert!(node.active && node.overloaded);
    assert_eq!(current.projection_revision, Some(7));
    assert!(!current.nodes[&Key::Ship].cargo_unlimited);
    assert_eq!(current.override_state(Key::Ship, infinity_core::consts::dogma::ATTR_CAPACITY),
        Some(crate::dao::items::dogma::OverrideState { value: 400.0, cargo_unlimited: false }));
}

#[cfg(feature = "tests-effects")] #[test]
fn repaired_modules_preserves_runtime_and_retries_zero_damage() {
    let mut current = module_fit(crate::sde::types::tests::combat_fixture(), 1, 1, true);
    current.nodes.get_mut(&Key::Ship).unwrap().overrides = vec![(ATTR_STRUCTURE_HP, 200.0)];
    let module = current.nodes.get_mut(&Key::Module(2)).unwrap();
    module.overrides = vec![(ATTR_HULL_DAMAGE, 8.0), (ATTR_STRUCTURE_HP, 200.0)];
    current.set_module_active(2, true);
    current.set_module_overloaded(2, true);
    current.projection_revision = Some(7);
    current.apply_committed_fitting(&CommittedFitting::repaired_modules([2].into_iter()));
    current.apply_committed_fitting(&CommittedFitting::repaired_modules([2].into_iter()));
    let module = &current.nodes[&Key::Module(2)];
    assert_eq!(module.overrides, vec![(ATTR_HULL_DAMAGE, 0.0), (ATTR_STRUCTURE_HP, 200.0)]);
    assert!(module.online && module.active && module.overloaded);
    assert_eq!(current.projection_revision, Some(7));
    assert_eq!(current.nodes[&Key::Ship].overrides, vec![(ATTR_STRUCTURE_HP, 200.0)]);
}
#[cfg(feature = "tests-effects")] #[test]
fn topology_offline_and_deterministic_candidates() {
    let mut before = fit();
    let mut current = fit();
    let mut final_fit = fit();
    for f in [&mut before, &mut current, &mut final_fit] { module(f, 2, true, vec![]); }
    module(&mut before, 3, true, vec![]);
    module(&mut current, 3, true, vec![]);
    module(&mut final_fit, 4, true, vec![]);
    current.set_module_active(2, true);
    current.set_module_overloaded(2, true);
    final_fit.set_module_online(2, false);
    let candidates = before.fitting_candidates(&final_fit);
    assert!(candidates.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(!candidates.iter().any(|(key, _)| *key == Key::Module(3)));
    assert!(candidates.iter().any(|(key, _)| *key == Key::Module(4)));
    let patch = final_fit.into_committed_fitting(&before);
    assert_eq!(patch.candidates(), candidates);
    current.apply_committed_fitting(&patch);
    assert!(!current.nodes.contains_key(&Key::Module(3)));
    assert!(current.nodes.contains_key(&Key::Module(4)));
    let node = &current.nodes[&Key::Module(2)];
    assert!(!node.online && !node.active && !node.overloaded);
}

#[cfg(feature = "tests-effects")] #[test]
fn current_projection_and_charge_topology_are_independent() {
    let mut before = fit();
    let mut current = fit();
    let mut final_fit = fit();
    for f in [&mut before, &mut current, &mut final_fit] { module(f, 2, true, vec![]); }
    module(&mut final_fit, 3, true, vec![]);
    let mut charge = Node {
        type_id: 1, data_position: current.sde.types.position(1), location: Location::Ship,
        online: true, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides: vec![], cargo_unlimited: false, charge_values: vec![],
    };
    current.nodes.insert(Key::Charge(2), charge);
    charge = Node {
        type_id: 1, data_position: current.sde.types.position(1), location: Location::Space,
        online: true, active: true, overloaded: false, projected_effect: None, receiver: Some(Key::Ship),
        charge_type: None, level: None, overrides: vec![], cargo_unlimited: false, charge_values: vec![],
    };
    current.nodes.insert(Key::Projected(9), charge);
    current.projection_revision = Some(4);
    current.apply_committed_fitting(&final_fit.into_committed_fitting(&before));
    assert!(current.nodes.contains_key(&Key::Charge(2)));
    assert!(current.nodes.contains_key(&Key::Projected(9)));
    assert_eq!(current.projection_revision, Some(4));
}

#[cfg(feature = "tests-effects")] #[test]
fn charge_incarnations_and_drone_nodes_track_committed_patches() {
    let node = |location| Node {
        type_id: 9, data_position: None, location, online: true, active: false,
        overloaded: false, projected_effect: None, receiver: None, charge_type: None,
        level: None, overrides: vec![], cargo_unlimited: false, charge_values: vec![],
    };
    // A cold charge insertion through the committed patch receives a fresh
    // incarnation; stale and wrong-type removals are refused.
    let mut current = fit();
    module(&mut current, 2, true, vec![]);
    let mut loaded = fit();
    module(&mut loaded, 2, true, vec![]);
    loaded.nodes.insert(Key::Charge(2), node(Location::Ship));
    current.apply_committed_fitting(&loaded.into_committed_fitting(&current));
    let (incarnation, type_id) = current.charge_incarnation(2).expect("resident charge token");
    assert_eq!(type_id, 9);
    assert!(!current.remove_charge(2, incarnation.wrapping_add(1), 9));
    assert!(!current.remove_charge(2, incarnation, 8));
    assert!(current.remove_charge(2, incarnation, 9));
    assert!(current.charge_incarnation(2).is_none());
    // A same-type reload can never alias the consumed token.
    let mut reloaded = fit();
    module(&mut reloaded, 2, true, vec![]);
    reloaded.nodes.insert(Key::Charge(2), node(Location::Ship));
    current.apply_committed_fitting(&reloaded.into_committed_fitting(&current));
    let (next, _) = current.charge_incarnation(2).expect("reloaded charge token");
    assert_ne!(next, incarnation);
    assert!(!current.remove_charge(2, incarnation, 9));
    assert!(current.remove_charge(2, next, 9));
    // A propagated removal drops the node by module and type only, never
    // matching this fit's own incarnation token.
    let mut reloaded = fit();
    module(&mut reloaded, 2, true, vec![]);
    reloaded.nodes.insert(Key::Charge(2), node(Location::Ship));
    current.apply_committed_fitting(&reloaded.into_committed_fitting(&current));
    assert!(current.remove_charge_node(2, 9));
    assert!(current.charge_incarnation(2).is_none());
    assert!(!current.remove_charge_node(2, 9));
    // Drone nodes merge and remove through the same committed patch shape.
    let mut before = fit();
    before.nodes.insert(Key::Drone(3), Node { overrides: vec![(5, 1.0)], ..node(Location::Space) });
    let mut after = fit();
    after.nodes.insert(Key::Drone(3), Node { overrides: vec![(5, 2.0)], ..node(Location::Space) });
    let mut current = fit();
    current.nodes.insert(Key::Drone(3), Node { overrides: vec![(5, 1.0)], ..node(Location::Space) });
    current.apply_committed_fitting(&after.into_committed_fitting(&before));
    assert_eq!(current.nodes[&Key::Drone(3)].overrides, vec![(5, 2.0)]);
    current.apply_committed_fitting(&fit().into_committed_fitting(&current));
    assert!(!current.nodes.contains_key(&Key::Drone(3)));
    // The ship-domain apply can never touch the receiver's own drone node.
    current.nodes.insert(Key::Drone(3), node(Location::Space));
    module(&mut current, 2, true, vec![]);
    let mut before = fit();
    module(&mut before, 2, true, vec![]);
    before.nodes.insert(Key::Drone(3), node(Location::Space));
    assert!(current.apply_ship_fitting(&fit().into_committed_fitting(&before)));
    assert!(current.nodes.contains_key(&Key::Drone(3)) && !current.nodes.contains_key(&Key::Module(2)));
    // A committed override delta sets, resets and clears only the named
    // fields; another item's keys never pass the drone confinement gate.
    let patch = CommittedFitting::overridden_attributes(vec![(Key::Drone(3), ATTR_STRUCTURE_HP, Some(50.0))]);
    assert!(patch.overrides_only_on(Key::Drone(3)) && !patch.overrides_only_on(Key::Module(2)));
    assert!(patch.candidates().contains(&(Key::Drone(3), ATTR_STRUCTURE_HP)));
    current.apply_committed_fitting(&patch);
    assert_eq!(current.nodes[&Key::Drone(3)].overrides, vec![(ATTR_STRUCTURE_HP, 50.0)]);
    current.apply_committed_fitting(&CommittedFitting::overridden_attributes(vec![(Key::Drone(3), ATTR_STRUCTURE_HP, None)]));
    assert!(current.nodes[&Key::Drone(3)].overrides.is_empty());
}

#[cfg(feature = "tests-effects")] #[test]
fn generic_patch_never_covers_loaded_quantity() {
    // A committed override-only delta keeps quantity out of both the applied
    // node delta's evaluation candidates and the fit's loaded seeds; exact i64
    // quantities are owned by the loaded absolute path alone.
    let mut current = fit();
    module(&mut current, 2, true, vec![]);
    current.set_loaded_seed(crate::space::combat::loaded::LoadedSeed {
        module: 2, flag: 27, type_id: Some(9), quantity: 8, pending: false });
    let patch = CommittedFitting::overridden_attributes(vec![
        (Key::Charge(2), ATTR_QUANTITY, Some(50.0)),
        (Key::Charge(2), ATTR_RADIUS, Some(50.0)),
    ]);
    assert!(!patch.candidates().contains(&(Key::Charge(2), ATTR_QUANTITY)));
    current.apply_committed_fitting(&patch);
    assert_eq!(current.loaded_seeds().iter().find(|s| s.module == 2).unwrap().quantity, 8);
    assert_eq!(current.loaded_seeds().len(), 1);
}

#[cfg(feature = "tests-effects")] #[tokio::test]
#[ignore = "requires a dedicated database with the SDE"]
async fn removed_modifier_and_typed_physical_candidates() {
    use std::sync::Arc;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|data| data.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
        && data.mass.is_some() && data.volume.is_some() && data.radius.is_some()).unwrap().type_id;
    let empty = Fit::for_space_type(sde.clone(), hull, vec![]);
    let physical = empty.fitting_candidates(&empty);
    for attr in [ATTR_MASS, ATTR_VOLUME, ATTR_RADIUS] {
        assert!(physical.contains(&(Key::Ship, attr)));
    }
    let mut found = false;
    for data in sde.types.iter() {
        let mut before = Fit::for_space_type(sde.clone(), hull, vec![]);
        module(&mut before, 2, true, vec![]);
        let source = before.nodes.get_mut(&Key::Module(2)).unwrap();
        source.type_id = data.type_id;
        source.data_position = sde.types.position(data.type_id);
        source.active = true;
        source.overloaded = true;
        let candidates = before.fitting_candidates(&empty);
        let old = before.fitting_values(&candidates);
        let changes = empty.fitting_changes(&old);
        if changes.iter().any(|&(key, attr, _, _)| key == Key::Ship
            && sde.types.attribute(hull, attr).is_none()) {
            let patch = empty.into_committed_fitting(&before);
            assert!(patch.candidates().iter().any(|pair| changes.iter().any(|change| (change.0, change.1) == *pair)));
            found = true;
            break;
        }
    }
    assert!(found, "SDE must contain a removed modifier-only ship target");
}
