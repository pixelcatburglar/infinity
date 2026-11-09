//! UUID text encoding shared by the gateway and static-data consumers.
//! RFC 9562 section 4 defines the textual representation; no data source or
//! client module is required. Version and variant semantics are not validated.
use std::fmt::Write;
use infinity_core::consts::UUID_LEN;

/// RFC 9562 section 4: hexadecimal groups in 8-4-4-4-12 form.
const UUID_HYPHENS: [usize; 4] = [8, 13, 18, 23];
/// RFC 9562 section 4: 32 hexadecimal digits and four hyphens.
const UUID_TEXT_LEN: usize = UUID_LEN * 2 + UUID_HYPHENS.len();

pub(crate) enum Case { Lowercase, Either }

pub(crate) fn parse(value: &str, case: Case) -> Option<[u8; UUID_LEN]> {
    let text = value.as_bytes();
    if text.len() != UUID_TEXT_LEN { return None; }
    let mut bytes = [0; UUID_LEN];
    let mut digits = 0;
    for (index, &c) in text.iter().enumerate() {
        if UUID_HYPHENS.contains(&index) {
            if c != b'-' { return None; }
            continue;
        }
        let nibble = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' if matches!(case, Case::Either) => c - b'A' + 10,
            _ => return None,
        };
        if digits % 2 == 0 { bytes[digits / 2] = nibble << 4; } else { bytes[digits / 2] |= nibble; }
        digits += 1;
    }
    Some(bytes)
}

pub(crate) fn format(bytes: &[u8; UUID_LEN]) -> String {
    let mut text = String::with_capacity(UUID_TEXT_LEN);
    for byte in bytes {
        if UUID_HYPHENS.contains(&text.len()) { text.push('-'); }
        write!(&mut text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

#[cfg(test)] mod tests;
