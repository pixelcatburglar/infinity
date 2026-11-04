use super::*;

#[test]
fn consume_stack_change_guards_old_quantity_and_leaves_one() {
    let item = Item {
        item_id: 17, type_id: 42, owner_id: 9, location_id: 11, flag_id: 5,
        quantity: 4, singleton: false, group_id: 0, category_id: 0, custom_info: None,
    };
    let change = QuantityChange::new(&item, Some(item.quantity - 1));
    assert_eq!(change.expected_quantity, 4);
    assert_eq!(change.quantity, Some(3));
    assert_eq!(item.quantity, 4);
}
