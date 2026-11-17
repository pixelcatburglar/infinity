use super::*;

fn dict(entries: Vec<(Value, Value)>) -> Value { Value::Dict(entries) }
fn valid(ty: i64, spacing: i64, size: i64) -> Value {
    dict(vec![(Value::str("formationType"), Value::Int(ty)),
        (Value::str("formationSpacing"), Value::Int(spacing)),
        (Value::str("formationSize"), Value::Int(size))])
}
fn tweak(ty: Value) -> Value {
    dict(vec![(Value::str("formationType"), ty),
        (Value::str("formationSpacing"), Value::Int(1_000)), (Value::str("formationSize"), Value::Int(10_000))])
}

#[test]
fn decodes_all_kinds_and_independent_tiers() {
    let kinds = [FormationKind::Point, FormationKind::Sphere, FormationKind::Plane,
        FormationKind::Wall, FormationKind::Arrow, FormationKind::Relative];
    for (i, kind) in kinds.into_iter().enumerate() {
        let s = decode(Some(&valid(i as i64, 10_000, 10_000))).unwrap();
        assert_eq!(s, FormationSettings { kind, spacing_m: 10_000.0, size_m: 10_000.0 });
        assert_eq!(requirements(&s), (i as i32, 5));
    }
    assert_eq!(decode(None).unwrap(), FormationSettings::default());
    assert_eq!(requirements(&decode(Some(&valid(0, 1_000, 10_000))).unwrap()), (0, 0));
    assert_eq!(requirements(&decode(Some(&valid(0, 1_000, 40_000))).unwrap()), (0, 3));
    assert_eq!(requirements(&decode(Some(&valid(0, 3_000, 50_000))).unwrap()), (0, 4));
}

#[test]
fn rejects_invalid_dicts_and_values() {
    for v in [
        Value::None, Value::Int(0), dict(vec![]),
        tweak(Value::Bool(true)), tweak(Value::Float(0.0)), tweak(Value::None), tweak(Value::str("0")),
        dict(vec![(Value::str("formationType"), Value::Int(0))]),
        dict(vec![(Value::Int(0), Value::Int(0)), (Value::str("formationSpacing"), Value::Int(1_000)),
            (Value::str("formationSize"), Value::Int(10_000))]),
        dict(vec![(Value::str("formationType"), Value::Int(0)), (Value::str("formationType"), Value::Int(0)),
            (Value::str("formationSpacing"), Value::Int(1_000)), (Value::str("formationSize"), Value::Int(10_000))]),
        dict(vec![(Value::str("formationType"), Value::Int(0)), (Value::str("formationSpacing"), Value::Int(1_000)),
            (Value::str("formationSize"), Value::Int(10_000)), (Value::str("unused"), Value::Int(0))]),
        valid(-1, 1_000, 10_000), valid(6, 1_000, 10_000),
        valid(0, 6, 10_000), valid(0, 999, 10_000), valid(0, -1_000, 10_000),
        valid(0, 1_000, 6), valid(0, 1_000, 999), valid(0, 1_000, -10_000),
    ] { assert!(decode(Some(&v)).is_err(), "{v:?}"); }
}
