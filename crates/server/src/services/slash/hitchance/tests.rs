use super::parse;
mod db_smoke;

#[test]
fn target_argument_requires_one_integer() {
    assert_eq!(parse(&["123".into()]), Some(123));
    assert_eq!(parse(&[]), None);
    assert_eq!(parse(&["me".into()]), None);
    assert_eq!(parse(&["0".into()]), None);
    assert_eq!(parse(&["-1".into()]), None);
    assert_eq!(parse(&["1".into(), "2".into()]), None);
}

#[test]
fn diagnostic_keeps_double_precision() {
    let value = 1.234_567_890_123_456_7_f64;
    let tiny = 2.0_f64.powi(-100);
    let row = infinity_core::messages::gm::gm_hitchance_row(1, 2, value, value, value, value, value, value, value, value, value, value, tiny);
    let printed = row.split('=').nth(1).unwrap().split('，').next().unwrap().parse::<f64>().unwrap();
    let printed_chance = row.rsplit('=').next().unwrap().parse::<f64>().unwrap();
    assert_eq!(printed, value);
    assert_eq!(printed_chance, tiny);
}
