use super::*;
use crate::isk::AmountError;

#[test]
fn plain_isk_text_preserves_progress_source() {
    assert_eq!(parse_isk("1.25").unwrap(), IskAmount::parse_decimal("1.25").unwrap());
    let db::Error::Progress(e) = parse_isk("1.001").unwrap_err() else { panic!("progress") };
    assert_eq!(e.to_string(), "invalid or out-of-range ISK amount");
    assert!(e.downcast_ref::<AmountError>().is_some());
}
