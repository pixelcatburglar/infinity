//! Pure SHA-256 identity hashes over supplied byte buffers or chunks for shared
//! settings and content; no client service or database is involved. Hashes use
//! lowercase hexadecimal and do not authenticate callers or confer authority.

use std::fmt::Write;
use sha2::{Digest, Sha256};

/// Two hexadecimal digits encode each byte.
const HEX_DIGITS_PER_BYTE: usize = 2;

/// Return the SHA-256 digest of supplied bytes as lowercase hexadecimal.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    sha256_hex_chunks([bytes])
}

/// Return the SHA-256 digest of sequential byte chunks as lowercase hexadecimal.
pub(crate) fn sha256_hex_chunks<I, B>(chunks: I) -> String
where
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let mut hasher = Sha256::new();
    for chunk in chunks {
        hasher.update(chunk.as_ref());
    }
    let digest = hasher.finalize();
    let mut result = String::with_capacity(digest.len() * HEX_DIGITS_PER_BYTE);
    for byte in digest { write!(&mut result, "{byte:02x}").expect("writing to a String cannot fail"); }
    result
}

#[cfg(all(test, feature = "tests-runtime"))] mod tests;
