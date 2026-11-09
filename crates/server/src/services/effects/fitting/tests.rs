//! Boundary checks for fitting resource and cascade selection.
use super::*;

mod db;
/// SDE effect category for modifiers active while a module is online.
const ONLINE_CATEGORY: i32 = 4;


pub(in crate::services) fn shield_extender(sde: &Sde) -> (i32, i32, bool) {
    let target = ATTR_SHIELD_CAPACITY;
    let positive = |ty: &crate::sde::types::TypeData, category: i32| {
        ty.effects().iter().any(|entry| sde.dogma.effect(entry.effect_id).is_some_and(|effect|
            effect.effect_category.unwrap_or(0) == category && effect.modifiers().iter().any(|m|
                m.func == "ItemModifier" && m.domain == "shipID" && m.modified == target
                    && matches!(m.op, crate::services::fit::OP_MOD_ADD | crate::services::fit::OP_POST_PERCENT | crate::services::fit::OP_POST_PERCENT_UNNERFED)
                    && ty.attribute(m.modifying).is_some_and(|v| v > 0.0))))
    };
    let module = sde.types.iter().find(|ty|
        ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
            && rack(&sde.types, ty.type_id).is_some_and(|(_, attr, _)| attr == ATTR_MED_SLOTS)
            && CAN_FIT_GROUPS.iter().chain(CAN_FIT_TYPES.iter()).all(|a| ty.attribute(*a).unwrap_or(0.0) == 0.0)
            && (positive(ty, 0) != positive(ty, ONLINE_CATEGORY))
            && (!positive(ty, ONLINE_CATEGORY) || ty.effects().iter().any(|entry| i64::from(entry.effect_id) == EFFECT_ONLINE)))
        .expect("published unrestricted shield extender in SDE");
    (module.type_id, rack(&sde.types, module.type_id).unwrap().2, !positive(module, 0))
}

#[test]
fn rig_slot_vacancy_respects_full_capacity_and_gaps() {
    let first = FLAG_RIG_FIRST;
    assert_eq!(free_rack_slot(first, 0, |_| false), None);
    assert_eq!(free_rack_slot(first, 3, |_| false), Some(first));
    assert_eq!(free_rack_slot(first, 3, |flag| flag == first || flag == first + 1), Some(first + 2));
    assert_eq!(free_rack_slot(first, 3, |flag| flag == first || flag == first + 2), Some(first + 1));
    assert_eq!(free_rack_slot(first, 3, |_| true), None);
}

#[test]
fn online_tolerance_and_zero_need() {
    assert!(!resource_exceeded(1.0, 10.0 + 0.5e-6, 10.0));
    assert!(resource_exceeded(1.0, 10.0 + 2e-6, 10.0));
    assert!(!resource_exceeded(0.0, 20.0, 10.0));
}

#[test]
fn cascade_uses_worst_ratio_with_cpu_tie_and_floor() {
    assert_eq!(worse_resource(10.0, 10.0, 5.0, 10.0), None);
    assert_eq!(worse_resource(12.0, 10.0, 12.0, 10.0), Some(ATTR_CPU));
    assert_eq!(worse_resource(11.0, 10.0, 12.0, 10.0), Some(ATTR_POWER));
    assert_eq!(worse_resource(0.0, 0.0, 1.0, 0.0), Some(ATTR_POWER));
}

#[test]
fn cascade_prioritizes_ordinary_then_usage_then_item_id() {
    assert_eq!(most_resource([(false, 100.0, 4), (true, 2.0, 2), (true, 2.0, 3), (true, 0.0, 99)].into_iter()), Some(3));
    assert_eq!(most_resource([(true, 0.0, 1)].into_iter()), None);
    assert_eq!(most_resource([(false, 4.0, 1), (false, 5.0, 2)].into_iter()), Some(2));
}
