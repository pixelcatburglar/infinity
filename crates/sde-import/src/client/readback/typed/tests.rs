use super::*;
fn decoded(family: &str, rows: &[&str]) -> super::super::Decoded {
    let mut compat_raw = BTreeMap::new();
    compat_raw.insert(family.into(), rows.iter().map(|s| s.to_string()).collect());
    super::super::Decoded { native: BTreeMap::new(), native_raw: BTreeMap::new(), compat_raw }
}
#[test]
fn float_bits_and_exact_integer_conversion() {
    let zero = project(Some(&exact("-0.0").unwrap()), Kind::Float).unwrap();
    assert!(!equal(&zero, &Cell::Float(Some(0.0))));
    let subnormal = project(Some(&exact("5e-324").unwrap()), Kind::Float).unwrap();
    assert!(equal(&subnormal, &Cell::Float(Some(f64::from_bits(1)))));
    let big = project(Some(&exact("9007199254740993").unwrap()), Kind::Big).unwrap();
    assert!(matches!(big, Cell::Big(Some(9007199254740993))));
    assert!(matches!(project(Some(&exact("1e-400").unwrap()), Kind::Bool).unwrap(), Cell::Bool(Some(true))));
}
#[test]
fn effects_aggregate_without_reordering_modifiers() {
    let d = decoded("typedogma", &[r#"{"_key":7,"dogmaEffects":[{"effectID":3,"isDefault":0},{"effectID":3,"isDefault":1},{"effectID":5}]}"#]);
    let rows = nested(&d,"typedogma","dogmaEffects","effectID",TYPE_EFFECTS,false,true).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(matches!(rows[&vec![7,3]]["is_default"], Cell::Bool(Some(true))));
    assert!(matches!(rows[&vec![7,5]]["is_default"], Cell::Bool(Some(false))));
    let d = decoded("dogmaeffects", &[r#"{"_key":2,"modifierInfo":[{"operation":1},{"operation":-1}]}"#]);
    let rows = nested(&d,"dogmaeffects","modifierInfo","",MODIFIERS,true,false).unwrap();
    assert!(matches!(rows[&vec![2,0]]["operation"], Cell::Int(Some(1))));
    assert!(matches!(rows[&vec![2,1]]["operation"], Cell::Int(Some(-1))));
}
#[test]
fn blueprints_preserve_exact_json_and_absence() {
    let d = decoded("blueprints", &[r#"{"_key":8,"activities":{"manufacturing":{"n":9007199254740993}}}"#]);
    let rows = base(&d,"blueprints",BLUEPRINTS).unwrap();
    let Cell::Json(Some(actual)) = &rows[&vec![8]]["activities"] else { panic!("missing activities") };
    assert_eq!(*actual, exact(r#"{"manufacturing":{"n":9007199254740993.0}}"#).unwrap());
    assert_ne!(*actual, exact(r#"{"manufacturing":{"n":9007199254740992}}"#).unwrap());
    assert!(matches!(rows[&vec![8]]["max_production_limit"], Cell::Int(None)));
}

#[test]
fn new_type_and_stale_typed_projection() {
    let d = decoded("types", &[r#"{"_key":101,"groupID":2,"name":"new","mass":3.5}"#]);
    let rows = base(&d, "types", TYPES).unwrap();
    assert!(rows.contains_key(&vec![101]));
    assert!(!equal(&rows[&vec![101]]["mass"], &Cell::Float(Some(3.0))));
    assert!(!equal(&rows[&vec![101]]["name"], &Cell::Text(Some("old".into()))));
}

#[test]
fn numeric_boundaries_and_wrong_kinds_are_preflight_errors() {
    for (family, column, field) in [("types", TYPES, "graphicID"), ("types", TYPES, "portionSize"), ("blueprints", BLUEPRINTS, "maxProductionLimit")] {
        let good = format!(r#"{{"_key":1,"{field}":2147483647}}"#);
        let bad = format!(r#"{{"_key":1,"{field}":2147483648}}"#);
        assert!(base(&decoded(family, &[&good]), family, column).is_ok());
        let err = base(&decoded(family, &[&bad]), family, column).unwrap_err();
        assert!(format!("{err:#}").contains(field));
        let bad = format!(r#"{{"_key":1,"{field}":"4"}}"#);
        assert!(base(&decoded(family, &[&bad]), family, column).is_err());
    }
    for (quantity, valid) in [("9223372036854775807", true), ("9223372036854775808", false)] {
        let row = format!(r#"{{"_key":1,"materials":[{{"materialTypeID":2,"quantity":{quantity}}}]}}"#);
        assert_eq!(nested(&decoded("typematerials", &[&row]), "typematerials", "materials", "materialTypeID", MATERIALS, false, false).is_ok(), valid);
    }
    assert!(base(&decoded("types", &[r#"{"_key":1,"mass":1e309}"#]), "types", TYPES).is_err());
    assert!(base(&decoded("types", &[r#"{"_key":"bad"}"#]), "types", TYPES).is_err());
}

#[test]
fn blueprint_consumer_casts_preserve_raw_and_refuse_invalid_domains() {
    let good = r#"{"manufacturing":{"time":0.5,"materials":[{"typeID":2147483647,"quantity":9223372036854775807}],"skills":[{"typeID":1,"level":2147483647}],"products":[{"typeID":1,"probability":0.5}]},"unknown":[1]}"#;
    let value = exact(good).unwrap();
    let Cell::Json(Some(projected)) = project(Some(&value), Kind::Activities).unwrap() else { panic!("missing activities") };
    assert_eq!(projected, value);
    for bad in [
        r#"[]"#, r#"{"manufacturing":[]}"#, r#"{"manufacturing":{"time":"x"}}"#,
        r#"{"manufacturing":{"materials":null}}"#, r#"{"manufacturing":{"materials":[null]}}"#,
        r#"{"manufacturing":{"materials":[{"typeID":1,"quantity":1.0}]}}"#,
        r#"{"manufacturing":{"materials":[{"typeID":1,"quantity":9223372036854775808}]}}"#,
        r#"{"manufacturing":{"skills":[{"typeID":1,"level":2147483648}]}}"#,
        r#"{"manufacturing":{"products":[{"typeID":1,"probability":1e309}]}}"#,
        r#"{"manufacturing":{"products":[{"typeID":"1"}]}}"#,
    ] { assert!(project(Some(&exact(bad).unwrap()), Kind::Activities).is_err(), "{bad}"); }
    assert!(project(Some(&exact(r#"{"manufacturing":{"materials":[{"typeID":1,"quantity":1e2}]}}"#).unwrap()), Kind::Activities).is_ok());
}

#[test]
fn typed_expectation_is_constructed_once_and_owns_rows() {
    let mut d = decoded("types", &[r#"{"_key":1,"graphicID":7}"#]);
    for family in ["groups", "categories", "typematerials", "dogmaattributes", "dogmaeffects", "typedogma", "blueprints"] { d.compat_raw.insert(family.into(), vec![]); }
    let expected = prepare_typed(&d).unwrap();
    d.compat_raw.get_mut("types").unwrap()[0] = r#"{"_key":1,"graphicID":2147483648}"#.into();
    let rows = &expected.0.iter().find(|entry| entry.0 == "types").unwrap().4;
    assert!(matches!(rows[&vec![1]]["graphic_id"], Cell::Int(Some(7))));
}

#[test]
fn typed_floats_reject_underflow_without_rejecting_zero_or_subnormal() {
    for token in ["1e-400", "-1e-400", "1e-9999", "1e309"] {
        assert!(project(Some(&exact(token).unwrap()), Kind::Float).is_err(), "{token}");
    }
    for (token, bits) in [
        ("0", 0.0f64.to_bits()), ("-0", (-0.0f64).to_bits()),
        ("5e-324", f64::from_bits(1).to_bits()),
        ("2.2250738585072014e-308", f64::MIN_POSITIVE.to_bits()),
        ("1e308", 1e308f64.to_bits()),
    ] {
        let Cell::Float(Some(value)) = project(Some(&exact(token).unwrap()), Kind::Float).unwrap() else { panic!("not float") };
        assert_eq!(value.to_bits(), bits, "{token}");
    }
    for token in ["1e-400", "-1e-400"] {
        let row = format!(r#"{{"_key":7,"activities":{{"manufacturing":{{"time":{token}}}}}}}"#);
        assert!(base(&decoded("blueprints", &[&row]), "blueprints", BLUEPRINTS).is_err());
    }
}
