use super::*;

fn rules(pattern: &str) -> Rules {
    Rules { patterns: vec![Regex::new(&translate(pattern)).unwrap()], markers: vec![] }
}

fn call<'a>(args: &'a [Value], kwargs: &'a Value) -> Call<'a> {
    Call { service: "bannedwords", method: "check_search_words_allowed", args, kwargs, bound: false, bind_params: None }
}

#[test]
fn translates_ascii_shorthands_and_preserves_other_escapes() {
    for (pattern, yes, no) in [
        (r"\d", "1", "１"), (r"\D", "１", "1"),
        (r"\s", " ", "\u{3000}"), (r"\S", "\u{a0}", " "),
        (r"\w", "a", "é"), (r"\W", "é", "a"),
        (r"[\d]", "1", "１"), (r"[\D]", "１", "1"),
        (r"[\s]", " ", "\u{a0}"), (r"[\S]", "\u{3000}", " "),
        (r"[\w]", "_", "é"), (r"[\W]", "é", "_"),
        (r"[a\D]", "a", "1"), (r"[a\D]", "é", "1"),
        (r"[^\D]", "1", "é"), (r"[^\S]", " ", "é"),
        (r"[^\W]", "_", "é"), (r"[a\d]", "1", "é"),
        (r"[a\s]", "\t", "é"), (r"[a\w]", "_", "é"),
        (r"\bcat\b", " cat ", "scat"), (r"\Bcat", "scat", " cat"),
        (r"[\b]", "\u{8}", "b"),
    ] {
        let re = Regex::new(&translate(pattern)).unwrap();
        assert!(re.is_match(yes).unwrap(), "{pattern}");
        assert!(!re.is_match(no).unwrap(), "{pattern}");
    }
    assert!(!Regex::new(&translate(r"\B")).unwrap().is_match("").unwrap());
    assert_eq!(translate(r"\n\t\.\\d"), r"\n\t\.\\d");
}

#[test]
fn checks_embedded_negative_lookahead_and_overlapping_windows() {
    let rules = rules(r"bad(?!ger)");
    assert!(check_text("a bad thing", &rules).unwrap());
    assert!(!check_text("badger", &rules).unwrap());
    assert!(check_text(&format!("{}bad", "x".repeat(199)), &rules).unwrap());
    assert!(!check_text("ordinary", &rules).unwrap());
}

#[test]
fn validates_payload_and_uses_committed_rules() {
    assert!(compile_payloads(&[Value::list([])]).is_err());
    let invalid = Value::list([Value::dict([("regex", Value::str("("))])]);
    assert!(compile_payloads(&[invalid]).is_err());
    assert!(!load_rules().unwrap().patterns.is_empty());
}

#[test]
fn checks_all_arguments_and_custom_error() {
    let rules = rules("bad");
    let args = [Value::unicode("fine"), Value::unicode("bad")];
    let kwargs = Value::dict([("err_message", Value::str("SomeClientError"))]);
    let error = check(&call(&args, &kwargs), Ok(&rules)).unwrap_err();
    assert!(matches!(error, super::super::CallError::User { msg, .. } if msg == "SomeClientError"));
    let failure = "unavailable".to_owned();
    let error = check(&call(&args, &kwargs), Err(&failure)).unwrap_err();
    assert!(matches!(error, super::super::CallError::User { msg, .. } if msg == MSG_BANNED_WORDS_DOWN));
    let invalid = [Value::list([Value::str("bad")])];
    assert!(check(&call(&invalid, &kwargs), Ok(&rules)).is_err());
}

#[test]
fn strips_markup_without_discarding_unknown_attributes() {
    let rules = load_rules().unwrap();
    let stripped = strip_markers("a<br><font size=\"12\" unknown>bad</font><b>word</b>", &rules.markers).unwrap();
    assert_eq!(stripped, "aunknownbadword");
}
