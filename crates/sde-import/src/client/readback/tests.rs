use super::*;

fn record(out: &mut Readback, expected: Option<&str>, actual: Option<&str>) {
    let e = expected.map(|s| exact(s).unwrap());
    let a = actual.map(|s| exact(s).unwrap());
    out.record("types", "compat", "types", ReportKey::integer("42").unwrap(), e.as_ref(), a.as_ref());
}
fn output() -> Readback {
    let mut r = Readback::default(); r.families.insert("types".into(), FamilyReadback::default()); r
}
#[test]
fn exact_rows_report_stale_missing_extra_and_presence() {
    let mut r = output();
    record(&mut r, Some(r#"{"x":null,"a":[0,false]}"#), Some(r#"{"a":[false,0]}"#));
    assert_eq!(r.differences.len(), 3);
    assert!(r.differences.iter().any(|d| d.path == "/x" && d.expected_present && !d.actual_present));
    record(&mut r, Some(r#"{"_key":42}"#), None);
    record(&mut r, None, Some(r#"{"_key":43}"#));
    assert_eq!(r.differences.len(), 5);
    assert!(!r.passed());
}
#[test]
fn exact_decimal_and_keyset() {
    let mut r = output();
    record(&mut r, Some(r#"{"n":100000000000000000000.0}"#), Some(r#"{"n":1e20}"#));
    assert!(r.passed());
    record(&mut r, Some(r#"{"n":100000000000000000001}"#), Some(r#"{"n":100000000000000000000}"#));
    assert_eq!(r.differences.len(), 1);
    let rows = vec![r#"{"_key":9007199254740993}"#.into(), r#"{"_key":9007199254740993}"#.into()];
    assert!(expected_rows("types", &rows).is_err());
}

#[test]
fn later_error_retains_prior_mismatch() {
    let mut r = output();
    record(&mut r, Some(r#"{"_key":42,"x":1}"#), Some(r#"{"_key":42,"x":2}"#));
    finish(&mut r, Err(anyhow!("later decoding failure")));
    assert_eq!(r.differences.len(), 1);
    assert_eq!(r.differences[0].path, "/x");
    assert_eq!(r.terminal_error.as_deref(), Some("later decoding failure"));
    assert_eq!(r.families["types"].status, "failed");
    assert!(!r.passed());
}

#[test]
fn native_only_families_have_no_compat_query_target() {
    for family in FAMILIES {
        assert_eq!(raw_target(family, "native"), Some(("sde_native", family.native_table)));
        assert_eq!(raw_target(family, "compat"), family.compat_table.map(|table| ("sde_compat", table)));
    }
}
