//! Server-authored synthetic dogma effects for database-free fitting tests.
use super::*;

pub(crate) const OVERLOAD_EFFECT: i32 = 900_001;
pub(crate) const BONUS_ATTRIBUTE: i32 = 900_002;

pub(crate) fn overload_fixture() -> Dogma {
    let effect = Effect {
        effect_id: OVERLOAD_EFFECT, name: None, guid: None, effect_category: Some(5),
        duration_attribute_id: None, fitting_usage_chance_attribute_id: None,
        is_offensive: false, is_assistance: false, disallow_auto_repeat: false,
        electronic_chance: false, propulsion_chance: false, discharge_attribute_id: None,
        range_attribute_id: None, falloff_attribute_id: None, tracking_attribute_id: None,
        modifiers: vec![Modifier { func: "ItemModifier".into(), domain: "shipID".into(),
            op: 2, modified: infinity_core::consts::dogma::ATTR_ARMOR_HP, modifying: BONUS_ATTRIBUTE,
            group: None, skill: None, target_category: None }],
    };
    Dogma { effects: vec![effect], effect_ids: [(OVERLOAD_EFFECT, 0)].into_iter().collect(),
        ..Default::default() }
}
