use super::*;
mod unfit;

#[test]
fn ammunition_requires_matching_group_and_optional_size() {
    let weapon = |attribute| match attribute {
        604 => Some(42.0),
        CHARGE_SIZE => Some(2.0),
        _ => None,
    };
    assert!(charge_compatible(42, Some(2.0), weapon));
    assert!(!charge_compatible(43, Some(2.0), weapon));
    assert!(!charge_compatible(42, Some(1.0), weapon));
    assert!(!charge_compatible(42, None, weapon));
    assert!(charge_compatible(42, None, |attribute| (attribute == 604).then_some(42.0)));
}

#[test]
fn quantities_must_be_positive_and_fit_i32() {
    let parse = |text: &str| {
        let values = [text.to_string()];
        positive(&Args::new(&values), 0)
    };
    assert!(matches!(parse("1"), Ok(1)));
    assert!(parse("0").is_err());
    assert!(parse("-1").is_err());
    assert!(parse("2147483648").is_err());
    assert!(parse("x").is_err());
}

