use super::*;
#[test]
fn duration_ms_is_exact_decimal_milliseconds() {
    assert_eq!(duration_ms("1500ms"), Some(1500));
    assert_eq!((duration_ms("1.5s"), duration_ms("0.5m"), duration_ms("2h"), duration_ms("90")), (Some(1500), Some(30_000), Some(7_200_000), Some(90_000)));
    for bad in ["-1s", "+1s", "1e3", ".5s", "1.s", "1.0001s", "ms", "1x", "", "9223372036854775807h"] {
        assert_eq!(duration_ms(bad), None, "{bad}");
    }
}
