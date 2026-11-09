//! Synthetic static data shared by database-free combat tests.
//! No client data is copied; the single ship type supplies only combat attributes.

use std::sync::Arc;

use super::{TypeData, Types};
use infinity_core::consts::{CATEGORY_SHIP, dogma::*};
use crate::sde::Sde;

const TEST_TYPE_ID: i32 = 1;
const TEST_HITPOINTS: f64 = 100.0;
const TEST_RECHARGE_MS: f64 = 1000.0;
const TEST_RESONANCE: f64 = 1.0;

pub(crate) fn combat_fixture() -> Arc<Sde> {
    let mut attributes = vec![
        (ATTR_STRUCTURE_HP, TEST_HITPOINTS),
        (ATTR_ARMOR_HP, TEST_HITPOINTS),
        (ATTR_SHIELD_CAPACITY, TEST_HITPOINTS),
        (ATTR_CAPACITOR_CAPACITY, TEST_HITPOINTS),
        (ATTR_RECHARGE_RATE, TEST_RECHARGE_MS),
        (ATTR_SHIELD_RECHARGE_RATE, TEST_RECHARGE_MS),
        (ATTR_SHIELD_EM_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_ARMOR_EM_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_ARMOR_THERMAL_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_ARMOR_KINETIC_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_ARMOR_EXPLOSIVE_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_STRUCTURE_THERMAL_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_STRUCTURE_KINETIC_DAMAGE_RESONANCE, TEST_RESONANCE),
        (ATTR_STRUCTURE_EXPLOSIVE_DAMAGE_RESONANCE, TEST_RESONANCE),
    ];
    attributes.sort_unstable_by_key(|&(id, _)| id);
    let data = TypeData {
        type_id: TEST_TYPE_ID, group_id: None, category_id: Some(CATEGORY_SHIP),
        name: None, lower_name: None, mass: None, capacity: None, volume: None, radius: None,
        portion_size: None, base_price: None, published: None, market_group_id: None,
        race_id: None, icon_id: None, attributes, effects: Vec::new(),
        modifiers: Default::default(), materials: Vec::new(),
    };
    let types = Types {
        types: vec![data], by_id: [(TEST_TYPE_ID, 0)].into_iter().collect(),
        ..Default::default()
    };
    Arc::new(Sde { types, ..Default::default() })
}

/// Add synthetic types for actor placement tests without loading a database.
pub(crate) fn placement_fixture(groups: &[(i32, Option<i32>)]) -> Arc<Sde> {
    let mut sde = Arc::try_unwrap(combat_fixture()).ok().unwrap();
    for &(type_id, group_id) in groups {
        let data = TypeData {
            type_id, group_id, category_id: None,
            name: None, lower_name: None, mass: None, capacity: None, volume: None, radius: None,
            portion_size: None, base_price: None, published: None, market_group_id: None,
            race_id: None, icon_id: None, attributes: Vec::new(), effects: Vec::new(),
            modifiers: Default::default(), materials: Vec::new(),
        };
        sde.types.by_id.insert(type_id, sde.types.types.len());
        sde.types.types.push(data);
    }
    Arc::new(sde)
}

/// Add attributes to synthetic placement types, without loading client data.
pub(crate) fn attribute_fixture(groups: &[(i32, Option<i32>)], attributes: &[(i32, Vec<(i32, f64)>)]) -> Arc<Sde> {
    let mut sde = Arc::try_unwrap(placement_fixture(groups)).ok().unwrap();
    for (id, values) in attributes {
        let index = sde.types.by_id[id];
        sde.types.types[index].attributes = values.clone();
        sde.types.types[index].attributes.sort_unstable_by_key(|value| value.0);
    }
    Arc::new(sde)
}
/// Add synthetic capacity and volume fields to placement types.
pub(crate) fn capacity_fixture(groups: &[(i32, Option<i32>)], fields: &[(i32, Option<f64>, Option<f64>)], attributes: &[(i32, Vec<(i32, f64)>)]) -> Arc<Sde> {
    let mut sde = Arc::try_unwrap(attribute_fixture(groups, attributes)).ok().unwrap();
    for &(id, capacity, volume) in fields {
        let index = sde.types.by_id[&id];
        sde.types.types[index].capacity = capacity;
        sde.types.types[index].volume = volume;
    }
    Arc::new(sde)
}


/// A synthetic overload modifier raises the hull's armor hitpoints by 50.
pub(crate) fn overload_fixture() -> Arc<Sde> {
    use crate::sde::dogma::tests::{overload_fixture, BONUS_ATTRIBUTE, OVERLOAD_EFFECT};
    let mut sde = Arc::try_unwrap(placement_fixture(&[(2, None)])).ok().unwrap();
    let index = sde.types.by_id[&2];
    sde.types.types[index].attributes = vec![(BONUS_ATTRIBUTE, 50.0)];
    sde.types.types[index].effects = vec![super::TypeEffect { effect_id: OVERLOAD_EFFECT, is_default: false }];
    sde.dogma = overload_fixture();
    sde.types.index_modifiers(&sde.dogma);
    Arc::new(sde)
}
