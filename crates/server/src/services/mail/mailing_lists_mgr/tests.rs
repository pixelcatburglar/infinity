use super::*;

#[test]
fn strict_scalar_types_and_ranges() {
    assert!(integer(&Value::Bool(true)).is_err());
    assert!(id(&Value::Int(0)).is_err());
    assert_eq!(id(&Value::Int(1)).ok(), Some(1));
    assert!(super::super::text_argument(&Value::Int(1)).is_err());
    assert!(super::super::text_argument(&Value::Bytes(vec![0xff])).is_err());
    assert!(access(&Value::Int(2)).is_err());
    assert!(fee(&Value::Bool(false)).is_err());
    assert!(fee(&Value::Int(1)).is_err());
}

#[test]
fn batches_are_bounded_sorted_and_strict() {
    let input = Value::Tuple(vec![Value::Int(3), Value::Int(1), Value::Int(3)]);
    assert_eq!(batch(&input).ok(), Some(vec![1, 3]));
    assert!(batch(&Value::List(vec![Value::Bool(true)])).is_err());
    assert!(batch(&Value::List(vec![Value::Int(1); MAX_MEMBER_BATCH + 1])).is_err());
    assert_eq!(batch(&Value::List(vec![])).ok(), Some(vec![]));
}

#[test]
fn access_rejects_conflicting_duplicates() {
    let same = Value::Dict(vec![(Value::Int(2), Value::Int(1)), (Value::Int(2), Value::Int(1))]);
    assert_eq!(access_entries(&same).ok(), Some(vec![(2, 1)]));
    let conflict = Value::Dict(vec![(Value::Int(2), Value::Int(1)), (Value::Int(2), Value::Int(0))]);
    assert!(access_entries(&conflict).is_err());
    assert!(access_entries(&Value::Dict(vec![(Value::Bool(true), Value::Int(1))])).is_err());
    assert!(access_entries(&Value::Dict(vec![(Value::Int(2), Value::Int(1)); MAX_MEMBER_BATCH + 1])).is_err());
}

#[test]
fn owner_view_uses_owner_id_not_stored_role() {
    let list = List {
        list_id: 1, name_key: "key".into(), display_name: "Name".into(), owner_id: 42,
        default_access: 1, default_member_access: 1, welcome_title: None, welcome_body: None,
        deleted_at: None,
    };
    for role in [ROLE_MUTED, ROLE_OPERATOR, 1] {
        assert!(view(&list, role, 42) == keyval([
            ("id", Value::Int(1)),
            ("name", Value::unicode("key")),
            ("displayName", Value::unicode("Name")),
            ("isMuted", Value::Bool(false)),
            ("isOperator", Value::Bool(false)),
            ("isOwner", Value::Bool(true)),
        ]));
    }
    assert!(view(&list, ROLE_OWNER, 43) == keyval([
        ("id", Value::Int(1)),
        ("name", Value::unicode("key")),
        ("displayName", Value::unicode("Name")),
        ("isMuted", Value::Bool(false)),
        ("isOperator", Value::Bool(false)),
        ("isOwner", Value::Bool(false)),
    ]));
}
