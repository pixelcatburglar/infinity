//! Lowercase hexadecimal encoding of caller-supplied bytes for importer and
//! publisher identities. No client module or database is involved; decoding and
//! other encodings are outside this module's scope.

/// Two hexadecimal digits encode each byte.
const HEX_DIGITS_PER_BYTE: usize = 2;

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * HEX_DIGITS_PER_BYTE);
    for byte in bytes { write!(&mut result, "{byte:02x}").expect("writing to String cannot fail"); }
    result
}

#[cfg(test)]
mod tests;
