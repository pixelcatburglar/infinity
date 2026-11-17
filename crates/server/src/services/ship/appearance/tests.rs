//! Wire checks for the forced Python-long dirt timestamp encoding that
//! `blue.os.TimeDiffInMs` requires of `GetDirtTimestamp` and the slim dirtTime.
use super::*;

#[test]
fn dirt_timestamp_dumps_as_canonical_long() {
    let exact = [
        (0i64, &[][..]),
        (1, &[0x01][..]),
        (128, &[0x80, 0x00][..]),
        (i64::from(i32::MAX), &[0xFF, 0xFF, 0xFF, 0x7F][..]),
        (i64::from(i32::MIN), &[0x00, 0x00, 0x00, 0x80][..]),
    ];
    for (stamp, payload) in exact {
        let bytes = marshal::dumps(&dirt_timestamp_value(stamp));
        assert_eq!((bytes[5], usize::from(bytes[6])), (marshal::tag::LONG, payload.len()), "{stamp}");
        assert_eq!(&bytes[7..], payload, "{stamp}");
        assert_eq!(marshal::loads(&bytes).unwrap().as_int(), Some(stamp), "{stamp}");
    }
    for stamp in [i64::from(i32::MAX) + 1, 133_000_000_000_000_000] { // outside the i32 domain / a real FILETIME
        let bytes = marshal::dumps(&dirt_timestamp_value(stamp));
        assert_eq!((bytes[5], marshal::loads(&bytes).unwrap().as_int()), (marshal::tag::LONG, Some(stamp)), "{stamp}");
    }
}
