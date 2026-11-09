use super::*;
use crate::uuid_text::{self, Case};

#[test]
fn replay_and_all_identity_dimensions() {
    let id = goal(1, 100, "v1", "slot", 0);
    assert_eq!(id, goal(1, 100, "v1", "slot", 0));
    for other in [goal(2, 100, "v1", "slot", 0), goal(1, 101, "v1", "slot", 0), goal(1, 100, "v2", "slot", 0), goal(1, 100, "v1", "other", 0), goal(1, 100, "v1", "slot", 1)] {
        assert_ne!(id, other);
    }
    let bytes = uuid_text::parse(&id, Case::Lowercase).unwrap();
    let first = asset(1, 100, "v1", "slot", 0, &bytes, 0);
    assert_eq!(first, asset(1, 100, "v1", "slot", 0, &bytes, 0));
    assert_ne!(first, id);
    assert_ne!(first, asset(1, 100, "v1", "slot", 0, &bytes, 1));
    assert_ne!(first, asset(1, 100, "v1", "slot", 0, &[0; UUID_LEN], 0));
}

#[test]
fn entropy_uses_all_digest_bits_and_framed_inputs() {
    let digest = base(b"daily-goals-entropy-v1", i64::MAX, i64::MIN, "v", i64::MAX).finalize();
    let entropy = entropy(i64::MAX, i64::MIN, "v", i64::MAX);
    for (word, chunk) in entropy.iter().zip(digest.chunks_exact(8)) {
        assert_eq!(*word, u64::from_be_bytes(chunk.try_into().unwrap()));
    }
    assert_ne!(entropy, super::entropy(i64::MAX, i64::MIN, "v", i64::MAX - 1));
    assert_ne!(goal(1, 2, "ab", "c", 0), goal(1, 2, "a", "bc", 0));
}

#[test]
fn uuid_is_canonical_and_strict() {
    let bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 255];
    let text = uuid_text::format(&bytes);
    assert_eq!(text, "00010203-0405-0607-0809-0a0b0c0d0eff");
    assert_eq!(uuid_text::parse(&text, Case::Lowercase).unwrap(), bytes);
    for invalid in ["", "000102030405-0607-0809-0a0b0c0d0eff", "00010203-0405-0607-0809-0A0b0c0d0eff", "00010203-0405-0607-0809-0g0b0c0d0eff", "00010203_0405-0607-0809-0a0b0c0d0eff", "é0010203-0405-0607-0809-0a0b0c0d0eff"] {
        assert!(uuid_text::parse(invalid, Case::Lowercase).is_none());
    }
}

#[test]
fn authored_uuid_accepts_hex_case_but_keeps_format_strict() {
    let bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 255];
    let canonical = uuid_text::format(&bytes);
    assert_eq!(uuid_text::parse(&canonical, Case::Either).unwrap(), bytes);
    for text in [
        "00010203-0405-0607-0809-0A0B0C0D0EFF",
        "00010203-0405-0607-0809-0A0b0C0d0EfF",
    ] {
        assert!(uuid_text::parse(text, Case::Lowercase).is_none());
        assert_eq!(uuid_text::parse(text, Case::Either).unwrap(), bytes);
        assert_eq!(uuid_text::format(&uuid_text::parse(text, Case::Either).unwrap()), canonical);
    }
    for invalid in [
        "",
        "000102030405-0607-0809-0a0b0c0d0eff",
        "00010203_0405-0607-0809-0a0b0c0d0eff",
        "00010203-0405-0607-0809-0g0b0c0d0eff",
        "é0010203-0405-0607-0809-0a0b0c0d0eff",
        "é010203-0405-0607-0809-0a0b0c0d0eff",
    ] {
        assert!(uuid_text::parse(invalid, Case::Lowercase).is_none());
        assert!(uuid_text::parse(invalid, Case::Either).is_none());
    }
}
