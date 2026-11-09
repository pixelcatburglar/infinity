use super::*;

fn snapshot(revision: i64, changed_at: i64, future: Option<i64>, crossed: Option<i64>) -> BookSnapshot {
    BookSnapshot { rows: vec![], revision, changed_at, next_visible_expiry: future, latest_crossed_expiry: crossed }
}

fn state(value: &Value) -> &[Value] {
    let Value::Instance(class, state) = value else { panic!("expected instance") };
    assert_eq!(**class, Value::str("carbon.common.script.net.objectCaching.CachedMethodCallResult"));
    state.items().unwrap()
}

#[test]
fn wrapper_has_client_state_and_roundtrips_marshaled_inner() {
    let inner = Value::tuple([Value::Dict(vec![]), Value::list([Value::Int(7)])]);
    for regional in [false, true] {
        let wrapped = wrap(inner.clone(), &snapshot(4, 10, Some(20), None), regional);
        let decoded = marshal::loads(&marshal::dumps(&wrapped)).unwrap();
        let parts = state(&decoded);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].get("versionCheck"), Some(&Value::str("always")));
        let region_id = Value::str("regionid");
        assert_eq!(parts[0].get("sessionInfo"), if regional { Some(&region_id) } else { None });
        let Value::Bytes(bytes) = &parts[1] else { panic!("inner must be bytes") };
        assert_eq!(marshal::loads(bytes).unwrap(), inner);
        assert_eq!(parts[2], Value::list([Value::Int(10), Value::tuple([Value::Int(4), Value::Int(20)])]));
    }
}

#[test]
fn tokens_track_revision_expiry_empty_and_restart() {
    let token = |s: &BookSnapshot| state(&wrap(Value::Dict(vec![]), s, false))[2].clone();
    let initial = snapshot(3, 10, Some(20), None);
    assert_eq!(token(&initial), token(&snapshot(3, 10, Some(20), None)));
    assert_ne!(token(&initial), token(&snapshot(4, 11, Some(20), None)));
    assert_ne!(token(&initial), token(&snapshot(3, 10, Some(30), Some(20))));
    let empty = snapshot(3, 10, None, Some(20));
    assert_eq!(token(&empty), Value::list([Value::Int(20), Value::tuple([Value::Int(3), Value::None])]));
    assert_eq!(token(&empty), token(&snapshot(3, 10, None, Some(20))));
    assert_ne!(token(&initial), token(&empty));
}
