//! Database-free regression tests for committed fitting field merges.

use super::*;
use infinity_core::consts::dogma::{ATTR_HULL_DAMAGE, ATTR_STRUCTURE_HP};
use crate::services::fit::Location;

fn fit() -> Fit {
    Fit::for_space_type(crate::sde::types::tests::combat_fixture(), 1, vec![])
}

fn module(fit: &mut Fit, id: i64, online: bool, overrides: Vec<(i32, f64)>) {
    fit.nodes.insert(Key::Module(id), Node {
        type_id: 1, data_position: fit.sde.types.position(1), location: Location::Ship,
        online, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides, charge_values: vec![],
    });
}

/// Build a ship Fit with explicit synthetic attribute overrides.
pub(crate) fn ship_fit(overrides: &[(i32, f64)]) -> Fit {
    let mut fit = fit();
    let node = fit.nodes.get_mut(&Key::Ship).unwrap();
    node.overrides = overrides.to_vec();
    node.overrides.sort_unstable_by_key(|entry| entry.0);
    fit
}

/// Build a synthetic fitted module using real static effect data when supplied.
pub(crate) fn module_fit(sde: std::sync::Arc<crate::sde::Sde>, hull: i32, module_type: i32, online: bool) -> Fit {
    let mut fit = Fit::for_space_type(sde, hull, vec![]);
    fit.nodes.insert(Key::Module(2), Node {
        type_id: module_type, data_position: fit.sde.types.position(module_type), location: Location::Ship,
        online, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides: vec![], charge_values: vec![],
    });
    fit
}

#[test]
fn field_delta_preserves_runtime_and_unmodified_damage() {
    let mut before = fit();
    let mut current = fit();
    let mut final_fit = fit();
    for f in [&mut before, &mut current, &mut final_fit] {
        module(f, 2, true, vec![(ATTR_HULL_DAMAGE, 1.0), (ATTR_STRUCTURE_HP, 200.0)]);
    }
    current.nodes.get_mut(&Key::Module(2)).unwrap().overrides[0].1 = 8.0;
    current.set_module_active(2, true);
    current.set_module_overloaded(2, true);
    current.projection_revision = Some(7);
    final_fit.nodes.get_mut(&Key::Module(2)).unwrap().overrides.pop();
    let patch = final_fit.into_committed_fitting(&before);
    let old = current.fitting_values(patch.candidates());
    current.apply_committed_fitting(patch);
    let node = &current.nodes[&Key::Module(2)];
    assert_eq!(node.overrides, vec![(ATTR_HULL_DAMAGE, 8.0)]);
    assert!(node.active && node.overloaded);
    assert_eq!(current.projection_revision, Some(7));
    assert!(current.fitting_changes(&old).contains(&(Key::Module(2), ATTR_STRUCTURE_HP, 200.0, 100.0)));
}

#[test]
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
    current.apply_committed_fitting(patch);
    assert!(!current.nodes.contains_key(&Key::Module(3)));
    assert!(current.nodes.contains_key(&Key::Module(4)));
    let node = &current.nodes[&Key::Module(2)];
    assert!(!node.online && !node.active && !node.overloaded);
}

#[test]
fn current_projection_and_charge_topology_are_independent() {
    let mut before = fit();
    let mut current = fit();
    let mut final_fit = fit();
    for f in [&mut before, &mut current, &mut final_fit] { module(f, 2, true, vec![]); }
    module(&mut final_fit, 3, true, vec![]);
    let mut charge = Node {
        type_id: 1, data_position: current.sde.types.position(1), location: Location::Ship,
        online: true, active: false, overloaded: false, projected_effect: None, receiver: None,
        charge_type: None, level: None, overrides: vec![], charge_values: vec![],
    };
    current.nodes.insert(Key::Charge(2), charge);
    charge = Node {
        type_id: 1, data_position: current.sde.types.position(1), location: Location::Space,
        online: true, active: true, overloaded: false, projected_effect: None, receiver: Some(Key::Ship),
        charge_type: None, level: None, overrides: vec![], charge_values: vec![],
    };
    current.nodes.insert(Key::Projected(9), charge);
    current.projection_revision = Some(4);
    current.apply_committed_fitting(final_fit.into_committed_fitting(&before));
    assert!(current.nodes.contains_key(&Key::Charge(2)));
    assert!(current.nodes.contains_key(&Key::Projected(9)));
    assert_eq!(current.projection_revision, Some(4));
}

#[tokio::test]
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
