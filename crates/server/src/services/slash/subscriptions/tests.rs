//! Pure clone-grade slash grammar regressions.
use super::*;

#[test]
fn accepts_valid_clone_grade_arguments() {
    let cases: &[(&[&str], (i64, i16, Option<i64>, i64))] = &[
        (&["42", "alpha"], (42, rules::ALPHA, None, 0)),
        (&["42", "omega"], (42, rules::OMEGA, None, 0)),
        (&["42", "omega", "30"], (42, rules::OMEGA, Some(30), 0)),
        (&["42", "omega", "30", "0"], (42, rules::OMEGA, Some(30), 0)),
        (&["42", "omega", "30", "5"], (42, rules::OMEGA, Some(30), 5)),
    ];
    for (args, expected) in cases {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(matches!(parse(&args), Ok(actual) if actual == *expected), "{args:?}");
    }
}

#[test]
fn rejects_invalid_clone_grade_arguments() {
    let cases: &[&[&str]] = &[
        &[], &["42"], &["42", "omega", "1", "0", "extra"],
        &["0", "alpha"], &["-1", "alpha"], &["not-a-number", "alpha"],
        &["9223372036854775808", "alpha"],
        &["42", "unknown"], &["42", ""], &["42", "Omega"],
        &["42", "alpha", "1"], &["42", "alpha", "1", "0"],
        &["42", "omega", "0"], &["42", "omega", "-1"],
        &["42", "omega", "9223372036854775808"],
        &["42", "omega", "1", "-1"], &["42", "omega", "1", "9223372036854775808"],
        &["42", "omega", "", "0"],
    ];
    for args in cases {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        assert!(parse(&args).is_err(), "{args:?}");
    }
}
