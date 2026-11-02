use super::*;

fn list(id: i64) -> dao::TypeList {
    dao::TypeList { id, included_types: vec![1, 2], included_groups: vec![10, 11], included_categories: vec![20, 21], excluded_types: vec![2], excluded_groups: vec![11, 12], excluded_categories: vec![21, 22] }
}

#[test]
fn assembly_requires_every_requested_list() {
    let ids = HashSet::from([NON_TRADABLE_LIST, SOULBOUND_LIST, 999]);
    let error = match TypeLists::from_rows(&ids, vec![list(NON_TRADABLE_LIST)]) {
        Ok(_) => panic!("missing lists were accepted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(&SOULBOUND_LIST.to_string()));
    assert!(error.contains("999"));
    let error = match TypeLists::from_rows(&ids, vec![list(SOULBOUND_LIST), list(999)]) {
        Ok(_) => panic!("missing trade list was accepted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(&NON_TRADABLE_LIST.to_string()));
    assert!(TypeLists::from_rows(&ids, vec![list(NON_TRADABLE_LIST), list(SOULBOUND_LIST), list(999)]).is_ok());
}

#[test]
fn purchase_list_is_required_at_startup() {
    let ids = HashSet::from(REQUIRED_LISTS);
    let error = match TypeLists::from_rows(&ids, vec![list(NON_TRADABLE_LIST), list(SOULBOUND_LIST)]) {
        Ok(_) => panic!("missing purchase list was accepted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(&TYPE_LIST_SKILLS_AVAILABLE_FOR_PURCHASE.to_string()));
}

#[test]
fn membership_obeys_client_precedence() {
    let lists = TypeLists([(1, list(1))].into_iter().collect());
    assert!(lists.contains(1, 3, None, Some(20)));
    assert!(!lists.contains(1, 3, None, Some(21)));
    assert!(lists.contains(1, 3, Some(10), Some(21)));
    assert!(!lists.contains(1, 3, Some(11), Some(20)));
    assert!(lists.contains(1, 1, Some(12), Some(22)));
    assert!(!lists.contains(1, 2, Some(10), Some(20)));
    assert!(!lists.contains(1, 3, None, None));
    assert!(!lists.contains(99, 1, Some(10), Some(20)));
}

#[test]
fn compressor_lists_do_not_restrict_trade() {
    let lists = TypeLists([(999, list(999)), (NON_TRADABLE_LIST, list(NON_TRADABLE_LIST))].into_iter().collect());
    assert!(lists.contains(999, 1, None, None));
    assert!(lists.is_tradable(3, None, None));
    assert!(!lists.is_tradable(1, None, None));
    let only_compressor = TypeLists([(999, list(999))].into_iter().collect());
    assert!(only_compressor.is_tradable(1, None, None));
}
