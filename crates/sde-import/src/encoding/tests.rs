use sha2::{Digest, Sha256};

#[test]
fn sha256_is_lowercase_hex() {
    assert_eq!(super::hex(&Sha256::digest(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
}
