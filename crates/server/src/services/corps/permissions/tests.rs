//! Differential vectors for the corporation edit rules.
use super::{can_edit_base, can_edit_role};
use serde_json::json;

#[test]
fn client_edit_role_vectors() {
    let default = [128, 128, 128, 128];
    let cases = [
        ("ordinary_grant_global",128,false,false,false,false,false,0,10,10,default,true),
        ("missing_grant_global",128,false,false,false,false,false,0,10,10,[0,128,128,128],false),
        ("hq_only",128,false,false,false,false,false,1,10,10,[0,128,0,0],true),
        ("other_only",128,false,false,false,false,false,3,10,10,[0,0,0,128],true),
        ("base_same",128,false,false,false,false,false,2,10,10,default,true),
        ("base_different",128,false,false,false,false,false,2,10,11,default,false),
        ("base_director_override",128,false,false,false,false,true,2,10,11,[0;4],true),
        ("base_ceo_override",128,false,false,false,true,false,2,10,11,[0;4],true),
        ("target_ceo",128,false,true,false,true,true,0,10,10,default,false),
        ("target_director_non_ceo",128,false,false,true,false,true,0,10,10,default,false),
        ("target_director_ceo_other_role",128,false,false,true,true,true,0,10,10,default,false),
        ("target_director_ceo_director_bit",1,false,false,true,true,true,0,10,10,[1,0,0,0],true),
        ("grantable_non_director",128,true,false,false,false,false,0,10,10,default,false),
        ("grantable_director",128,true,false,false,false,true,0,10,10,default,true),
        ("grantable_target_director",128,true,false,true,false,true,0,10,10,default,false),
        ("grantable_director_bit",1,true,false,false,false,true,0,10,10,default,false),
        ("high_role_bit",1i64<<62,false,false,false,false,false,0,10,10,[1i64<<62,0,0,0],true),
    ];
    for (id, role, grantable, target_ceo, target_director, actor_ceo, actor_director, scope, actor_base, target_base, grants, expected) in cases {
        let actual = can_edit_role(role, grantable, target_ceo, target_director, actor_ceo, actor_director, scope, Some(actor_base), Some(target_base), grants);
        let scope_name = ["roles", "rolesAtHQ", "rolesAtBase", "rolesAtOther"][scope];
        println!("{}", json!({"id":id,"operation":"CanEditRole","input":{"role":role,"grantable":grantable,"target_ceo":target_ceo,"target_director":target_director,"actor_ceo":actor_ceo,"actor_director":actor_director,"scope":scope_name,"actor_base":actor_base,"target_base":target_base,"grants":grants},"result":actual}));
        assert_eq!(actual, expected, "{id}");
    }
    for (id, scope, grants, expected) in [
        ("real_hangar_hq", 1usize, [0, 8192, 0, 0], true),
        ("real_hangar_hq_base_only", 1usize, [0, 0, 8192, 0], false),
        ("real_hangar_base_same", 2usize, [0, 0, 8192, 0], true),
        ("real_hangar_other", 3usize, [0, 0, 0, 8192], true),
    ] {
        let actual = can_edit_role(8192, false, false, false, false, false, scope, Some(10), Some(10), grants);
        let scope_name = ["roles", "rolesAtHQ", "rolesAtBase", "rolesAtOther"][scope];
        println!("{}", json!({"id":id,"operation":"CanEditRole","input":{"role":8192,"grantable":false,"target_ceo":false,"target_director":false,"actor_ceo":false,"actor_director":false,"scope":scope_name,"actor_base":10,"target_base":10,"grants":grants},"result":actual}));
        assert_eq!(actual, expected, "{id}");
    }
    for (id, actor_ceo, actor_director, target_ceo, expected) in [
        ("base_False_False_False", false, false, false, false),
        ("base_False_False_True", false, true, false, true),
        ("base_False_True_False", true, false, false, true),
        ("base_True_False_True", false, true, true, true),
        ("base_True_False_False", false, false, true, false),
        ("base_True_True_False", true, false, true, true),
    ] {
        let actual = can_edit_base(actor_ceo, actor_director);
        println!("{}", json!({"id":id,"operation":"CanEditBase","input":{"actor_ceo":actor_ceo,"actor_director":actor_director,"target_ceo":target_ceo},"result":actual}));
        assert_eq!(actual, expected, "{id}");
    }
}

#[test]
fn location_scopes_do_not_leak_and_headquarters_precedes_base() {
    let mut authority = super::Authority {
        character: 1, corporation: 2, ceo: Some(3), headquarters: Some(10), live: true, base: Some(20), blocked: false,
        roles: [128, 2, 4, 8], grantable: [0; 4],
    };
    assert!(super::allows_at(&authority, 2, 10, 2));
    assert!(!super::allows_at(&authority, 2, 20, 2));
    assert!(super::allows_at(&authority, 2, 20, 4));
    assert!(super::allows_at(&authority, 2, 30, 8));
    assert!(!super::allows_at(&authority, 3, 10, 2));
    authority.base = Some(10);
    assert!(!super::allows_at(&authority, 2, 10, 4));
    authority.roles[0] = infinity_core::consts::ROLE_DIRECTOR;
    assert!(super::allows_at(&authority, 2, 30, 4));
    assert!(!super::active_ceo(&authority, 1));
    authority.ceo = Some(1);
    assert!(super::active_ceo(&authority, 1));
    authority.roles[0] = 0;
    assert!(!super::active_ceo(&authority, 1));
}
