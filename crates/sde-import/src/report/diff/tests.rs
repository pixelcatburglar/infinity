use super::*;

#[test]
fn decimal_exactness_and_typed_bits_are_independent() {
    for (a,b) in [("1", "1.0"), ("1e+3", "1000.00"), ("-0", "0e-400"), ("0.00001", "1e-5")] {
        assert_eq!(ExactNumber::parse(a).unwrap(), ExactNumber::parse(b).unwrap());
    }
    for (a,b) in [("9007199254740992", "9007199254740993"), ("18446744073709551616", "18446744073709551617"), ("1e-324", "2e-324")] {
        let a = ExactNumber::parse(a).unwrap(); let b = ExactNumber::parse(b).unwrap(); assert_ne!(a,b);
        assert_eq!(a.finite_f64().unwrap().to_bits(), b.finite_f64().unwrap().to_bits());
    }
    let negative = ExactNumber::parse("-0.0").unwrap(); let positive = ExactNumber::parse("0").unwrap();
    assert!(negative.negative_zero); assert_eq!(negative, positive); assert_ne!(negative.finite_f64().unwrap().to_bits(), positive.finite_f64().unwrap().to_bits());
    assert!(ExactNumber::parse("1.7976931348623157e308").unwrap().finite_f64().is_ok());
    assert!(ExactNumber::parse("1e309").unwrap().finite_f64().is_err());
    assert_ne!(ExactNumber::parse("5e-324").unwrap().finite_f64().unwrap().to_bits(), 0);
    for bad in ["", "01", "-", "1.", "1e", "+1", "1e999999999999999999999999", "1e-999999999999999999999999"] { assert!(ExactNumber::parse(bad).is_err(), "{bad}"); }
    assert!(ExactNumber::parse(&"1".repeat(MAX_NUMBER_BYTES + 1)).is_err());
}

#[test]
fn complete_structural_diff_and_exact_serialization() {
    let old = parse_json(r#"{"a/b":[null,0,false],"~x":9007199254740992}"#).unwrap();
    let new = parse_json(r#"{"a/b":[null,false,0,4],"~x":9007199254740993,"z":null}"#).unwrap();
    let d = compare("synthetic", ReportKey::integer("-0002").unwrap(), Some(&old), Some(&new), "source", "hash");
    assert_eq!(d.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(), ["/a~1b/1", "/a~1b/2", "/a~1b/3", "/z", "/~0x"]);
    assert!(!d[3].old_present && d[3].new_present);
    assert_eq!(d[4].numeric_classification, "f64-representation-only");
    assert!(serde_json::to_string(&d[4]).unwrap().contains("9007199254740993"));
    let added = compare("synthetic", ReportKey::String("x".into()), None, Some(&new), "source", "hash");
    assert_eq!(added.len(), 1); assert_eq!(added[0].kind, "client-only"); assert_eq!(added[0].new_value.as_ref(), Some(&new));
    assert!(parse_json(r#"{"x":1,"x":2}"#).is_err());
    for bad in ["[1,]", "{\"x\":}", "1x", "[true false]"] { assert!(parse_json(bad).is_err()); }
}

#[test]
fn key_order_is_mathematical_and_tagged() {
    let mut keys = vec![ReportKey::String("-1".into()), ReportKey::integer("100000000000000000000").unwrap(), ReportKey::integer("-2").unwrap(), ReportKey::integer("-10").unwrap(), ReportKey::integer("0").unwrap()];
    keys.sort();
    assert_eq!(keys, vec![ReportKey::integer("-10").unwrap(), ReportKey::integer("-2").unwrap(), ReportKey::integer("0").unwrap(), ReportKey::integer("100000000000000000000").unwrap(), ReportKey::String("-1".into())]);
    assert_eq!(serde_json::to_string(&keys[0]).unwrap(), r#"{"kind":"integer","value":"-10"}"#);
}

#[test]
fn postgres_integer_rounding_is_exact_and_checked() {
    for (token, expected) in [
        ("0.5", 1), ("-0.5", -1), ("0.4999", 0), ("-0.4999", 0),
        ("1.5", 2), ("-1.5", -2), ("5e-1", 1), ("-5e-1", -1),
        ("4.999e-1", 0), ("1e-1000", 0), ("12e2", 1200),
        ("9007199254740993", 9007199254740993), ("9223372036854775807.49", i64::MAX),
        ("-9223372036854775808.49", i64::MIN),
    ] {
        assert_eq!(ExactNumber::parse(token).unwrap().as_i64_rounded().unwrap(), expected, "{token}");
    }
    for token in ["9223372036854775807.5", "-9223372036854775808.5", "9223372036854775808", "-9223372036854775809", "1e1000"] {
        assert!(ExactNumber::parse(token).unwrap().as_i64_rounded().is_err(), "{token}");
    }
    for (token, expected) in [("2147483647.49", i32::MAX), ("-2147483648.49", i32::MIN), ("-0.5", -1)] {
        assert_eq!(ExactNumber::parse(token).unwrap().as_i32_rounded().unwrap(), expected);
    }
    for token in ["2147483647.5", "-2147483648.5"] {
        assert!(ExactNumber::parse(token).unwrap().as_i32_rounded().is_err());
    }
}

#[test]
fn jsonb_integer_text_cast_retains_decimal_scale() {
    for (text, value) in [("1e2", 100), ("1.2e1", 12), ("9223372036854775807", i64::MAX), ("-9223372036854775808", i64::MIN)] {
        assert_eq!(ExactNumber::parse(text).unwrap().as_jsonb_text_i64().unwrap(), value);
    }
    for text in ["1.0", "1e-1", "9223372036854775808", "-9223372036854775809"] {
        assert!(ExactNumber::parse(text).unwrap().as_jsonb_text_i64().is_err(), "{text}");
    }
}

#[test]
fn postgres_jsonb_numeric_storage_boundaries_preserve_scale_and_zero() {
    for token in ["1e131071", "1e-16383", "1e-9999", "0e-16383", "0e131072", "1.0e131071", "0e1073741823"] {
        assert!(ExactNumber::parse(token).unwrap().fits_postgres_numeric(), "{token}");
    }
    for token in ["1e131072", "1e-16384", "1e-20000", "0e-16384", "10e-16384", "0.0e-16384", "0e1073741824"] {
        assert!(!ExactNumber::parse(token).unwrap().fits_postgres_numeric(), "{token}");
    }
}
