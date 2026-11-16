use super::{current_system, no_character, positive_character, positive_i64, selected_character, tokenize, Args};
use infinity_core::session::Session;

fn session(pairs: &[(&str, i64)]) -> Session {
    let mut session = Session::new(0);
    session.apply(pairs.iter().map(|(key, value)| (*key, marshal::Value::Int(*value))).collect());
    session
}

#[test]
fn tokenizes_quotes_and_whitespace() {
    assert_eq!(tokenize("  /cmd \"multi word\" x\"y z\" \"\"\tend ").ok(),
        Some(vec!["/cmd".into(), "multi word".into(), "xy z".into(), "".into(), "end".into()]));
    assert_eq!(tokenize(" \t ").ok(), Some(Vec::new()));
    assert!(tokenize("one \"unterminated").is_err());
}

#[test]
fn parses_numeric_arguments_and_rejects_invalid_values() {
    let values = ["-42".into(), "1.25".into(), "NaN".into(), "inf".into(), "no".into(), "".into()];
    let args = Args::new(&values);
    assert_eq!(args.i64(0).ok(), Some(-42));
    assert_eq!(args.f64(1).ok(), Some(1.25));
    assert!(args.f64(2).is_err());
    assert!(args.f64(3).is_err());
    assert!(args.i64(4).is_err());
    assert!(args.required(5).is_err());
    assert!(args.required(6).is_err());
}

#[test]
fn positive_i64_accepts_only_positive_integer_text() {
    for (text, expected) in [("1", Some(1)), ("9223372036854775807", Some(i64::MAX)),
        ("0", None), ("-1", None), ("1.5", None), ("abc", None), ("", None), ("9223372036854775808", None)] {
        assert_eq!(positive_i64(text), expected, "{text}");
    }
}

#[test]
fn session_character_policies_share_the_no_character_error() {
    assert_eq!(selected_character(&session(&[("charid", 0)])).ok(), Some(0));
    assert_eq!(selected_character(&session(&[("charid", -5)])).ok(), Some(-5));
    assert_eq!(selected_character(&session(&[("charid", 42)])).ok(), Some(42));
    assert_eq!(format!("{:?}", selected_character(&session(&[])).unwrap_err()), format!("{:?}", no_character()));
    for pairs in [&[][..], &[("charid", 0)][..], &[("charid", -5)][..]] {
        assert_eq!(format!("{:?}", positive_character(&session(pairs)).unwrap_err()), format!("{:?}", no_character()));
    }
    assert_eq!(positive_character(&session(&[("charid", 42)])).ok(), Some(42));
}

#[test]
fn current_system_prefers_canonical_then_undocked_fallback() {
    assert_eq!(current_system(&session(&[("solarsystemid2", 5), ("stationid", 1), ("solarsystemid", 9)])), Some(5));
    assert_eq!(current_system(&session(&[("solarsystemid", 9)])), Some(9));
    assert_eq!(current_system(&session(&[("stationid", 1), ("solarsystemid", 9)])), None);
    assert_eq!(current_system(&session(&[])), None);
    assert_eq!(current_system(&session(&[("solarsystemid2", 0)])), Some(0));
    assert_eq!(current_system(&session(&[("solarsystemid", -4)])), Some(-4));
}
