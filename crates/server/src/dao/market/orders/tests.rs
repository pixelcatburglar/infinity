use infinity_core::consts::FILETIME_DAY;

#[test]
fn owned_order_deadline_is_strict_and_checked() {
    let issued = 10;
    let deadline = issued + FILETIME_DAY;
    let mut order = crate::dao::market::orders::Order {
        id: 1, type_id: 1, char_id: Some(1),
        at: crate::dao::market::orders::Station { id: 1, system: 1, constellation: 1, region: 1, owner: 1 },
        corporation_id: None, wallet_key: infinity_core::consts::ACCOUNT_CASH,
        source_location_id: None, source_flag_id: None,
        price: crate::isk::IskAmount::from_whole_isk(1).unwrap(),
        vol_entered: 1, vol_remaining: 1, min_volume: 1, bid: true, range: -1,
        duration: 1, issued: Some(issued), escrow: crate::isk::IskAmount::from_whole_isk(1).unwrap(),
        scope: crate::dao::market::orders::MarketScope::Regional, vault_user_id: None,
        plex_escrow: 0, created_at: Some(issued), expires_at: None,
    };
    assert!(order.owned_active_at(deadline - 1));
    assert!(!order.owned_active_at(deadline));
    assert!(!order.owned_active_at(deadline + 1));
    order.issued = None;
    assert!(!order.owned_active_at(deadline - 1));
    order.issued = Some(i64::MAX);
    assert!(order.owned_active_at(deadline));
    order.issued = Some(0);
    order.duration = i64::MAX;
    assert!(order.owned_active_at(deadline));
    order.expires_at = Some(deadline);
    assert!(!order.owned_active_at(deadline));
    order.issued = None;
    order.duration = 0;
    assert!(order.owned_active_at(deadline - 1));
    (order.issued, order.expires_at, order.duration) = (Some(0), None, i64::MIN);
    assert!(!order.owned_active_at(i64::MIN));
}
