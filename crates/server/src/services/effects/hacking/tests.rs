use super::*;
use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_FITTING_FIRST};
use crate::dao::items::loading::Item;
use crate::sde::types::TypeEffect;

#[test]
fn analyzer_uses_shared_fitted_online_subject_policy() {
    let hull = Item { item_id: 3, type_id: 1, owner_id: 2, location_id: 4, flag_id: 0, quantity: 1, singleton: true, group_id: 0, category_id: i64::from(CATEGORY_SHIP), custom_info: None };
    let mut module = Item { item_id: 5, type_id: 1, owner_id: 2, location_id: 3, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, group_id: 0, category_id: i64::from(CATEGORY_MODULE), custom_info: None };
    let effects = [TypeEffect { effect_id: EFFECT_HACKING, is_default: false }];
    assert!(lifecycle::valid_subject(&hull, &module, 2, 3, Some(3), None, &effects, EFFECT_HACKING, true, true));
    assert!(!lifecycle::valid_subject(&hull, &module, 2, 3, Some(3), None, &effects, EFFECT_HACKING, true, false));
    module.flag_id = 0;
    assert!(!lifecycle::valid_subject(&hull, &module, 2, 3, Some(3), None, &effects, EFFECT_HACKING, true, true));
}
