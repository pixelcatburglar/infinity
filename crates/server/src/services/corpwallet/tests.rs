use super::*;
use infinity_core::consts::ROLE_JUNIOR_ACCOUNTANT;
use crate::services::corps::permissions::Authority;

#[test]
fn withdrawal_requires_the_selected_take_role_or_director() {
    let corp = 98_000_000;
    let mut authority = Authority { character: 1, corporation: corp, ceo: Some(1), headquarters: None, live: true, base: None, blocked: false, roles: [0; 4], grantable: [0; 4] };
    for role in [ROLE_ACCOUNTANT, ROLE_JUNIOR_ACCOUNTANT] {
        authority.roles[0] = role;
        for key in ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS { assert!(!can_take(&authority, corp, key)); }
    }
    for division in 0..DIVISIONS {
        authority.roles[0] = ROLE_ACCOUNTANT | (ROLE_ACCOUNT_CAN_TAKE_1 << division);
        for key in ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS {
            assert_eq!(can_take(&authority, corp, key), key == ACCOUNT_CASH + division);
        }
    }
    authority.roles[0] = ROLE_DIRECTOR;
    for key in ACCOUNT_CASH..ACCOUNT_CASH + DIVISIONS { assert!(can_take(&authority, corp, key)); }
    assert!(!can_take(&authority, corp, ACCOUNT_CASH - 1));
    assert!(!can_take(&authority, corp, ACCOUNT_CASH + DIVISIONS));
    assert!(!can_take(&authority, 1, ACCOUNT_CASH));
    assert!(!can_take(&authority, corp + 1, ACCOUNT_CASH));
    authority.live = false;
    assert!(!can_take(&authority, corp, ACCOUNT_CASH));
}
