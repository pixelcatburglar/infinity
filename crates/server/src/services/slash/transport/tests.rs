use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn destination_id_accepts_lawful_forms() {
    for (words, expected) in [
        (&["123"][..], 123),
        (&["me", "123"][..], 123),
        (&["Me", "123"][..], 123),
        (&["ME", "123"][..], 123),
        (&["123", "noblock"][..], 123),
        (&["me", "123", "noblock"][..], 123),
        (&["me", "123", "NoBlock"][..], 123),
    ] {
        assert_eq!(destination_id(&args(words)).unwrap(), expected, "{words:?}");
    }
}

#[test]
fn destination_id_rejects_unsupported_forms() {
    // Missing or empty destination, name lookup, extra arguments and raw
    // coordinates are all refused rather than silently ignored.
    for words in [
        &[][..],
        &["me"][..],
        &["me", "me"][..],
        &["Jita"][..],
        &["0"][..],
        &["-5"][..],
        &["123", "456"][..],
        &["me", "123", "offset=5000"][..],
        &["me", "123", "noblock", "extra"][..],
        &["1", "2", "3"][..],
        &["me", "123", "me"][..],
        // `noblock` only as a trailing token, never leading or embedded.
        &["noblock", "123"][..],
        &["me", "noblock", "123"][..],
    ] {
        assert!(destination_id(&args(words)).is_err(), "{words:?}");
    }
}
