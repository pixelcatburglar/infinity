//! Pure SHA-256 identity hashes over supplied byte buffers for shared settings
//! and content; no client service or database is involved. Hashes use lowercase
//! hexadecimal and do not authenticate callers or confer authority.

use std::fmt::Write;
use sha2::{Digest, Sha256};

/// Two hexadecimal digits encode each byte.
const HEX_DIGITS_PER_BYTE: usize = 2;

/// Return the SHA-256 digest of supplied bytes as lowercase hexadecimal.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut result = String::with_capacity(digest.len() * HEX_DIGITS_PER_BYTE);
    for byte in digest { write!(&mut result, "{byte:02x}").expect("writing to a String cannot fail"); }
    result
}
