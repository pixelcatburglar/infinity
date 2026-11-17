use super::*;

#[test]
fn sha256_chunks_match_byte_buffer() {
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(sha256_hex(b"abc"), expected);
    assert_eq!(sha256_hex_chunks([&b"a"[..], &b"bc"[..]]), expected);
}
