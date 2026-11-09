use super::*;

#[test]
fn cases_and_roundtrips() {
    let bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 255];
    let text = format(&bytes);
    assert_eq!(text, "00010203-0405-0607-0809-0a0b0c0d0eff");
    assert_eq!(parse(&text, Case::Lowercase), Some(bytes));
    for mixed in ["00010203-0405-0607-0809-0A0B0C0D0EFF", "00010203-0405-0607-0809-0A0b0C0d0EfF"] {
        assert_eq!(parse(mixed, Case::Lowercase), None);
        assert_eq!(parse(mixed, Case::Either), Some(bytes));
    }
    for bytes in [[0; UUID_LEN], [255; UUID_LEN], bytes] {
        assert_eq!(parse(&format(&bytes), Case::Lowercase), Some(bytes));
        assert_eq!(parse(&format(&bytes), Case::Either), Some(bytes));
    }
}

#[test]
fn malformed_shapes_hex_and_unicode_are_rejected() {
    for invalid in ["", "000102030405-0607-0809-0a0b0c0d0eff", "00010203_0405-0607-0809-0a0b0c0d0eff", "00010203-0405-0607-0809-0g0b0c0d0eff", "00010203-0405-0607-0809-0-0b0c0d0eff", "é0010203-0405-0607-0809-0a0b0c0d0eff", "é010203-0405-0607-0809-0a0b0c0d0eff"] {
        assert_eq!(parse(invalid, Case::Lowercase), None, "{invalid}");
        assert_eq!(parse(invalid, Case::Either), None, "{invalid}");
    }
    for len in 0..=UUID_TEXT_LEN * 2 {
        assert_eq!(parse(&"0".repeat(len), Case::Lowercase), None);
        assert_eq!(parse(&"0".repeat(len), Case::Either), None);
    }
    for pos in UUID_HYPHENS {
        let mut text = format(&[0; UUID_LEN]).into_bytes();
        text[pos] = b'0';
        assert_eq!(parse(std::str::from_utf8(&text).unwrap(), Case::Either), None);
    }
}
