//! SQL LIKE patterns for slash type lookup and mission NPC selection.
//! These server-side SDE searches are not client-provided matching logic. Both
//! inputs must already be folded by PostgreSQL. LIKE defines `%`, `_`, and
//! backslash escapes; invalid trailing escapes do not match.

pub(super) fn matches_pattern(name: &str, pattern: &str) -> bool {
    if let Some(literal) = pattern.strip_prefix('%').and_then(|p| p.strip_suffix('%')) {
        if !literal.bytes().any(|b| matches!(b, b'%' | b'_' | b'\\')) {
            return name.contains(literal);
        }
    }

    let (mut n, mut p, mut star, mut retry) = (0, 0, None, 0);
    while n < name.len() {
        let pattern_char = pattern[p..].chars().next();
        if pattern_char == Some('%') {
            p += 1;
            star = Some(p);
            retry = n;
        } else {
            let escaped = pattern_char == Some('\\');
            let literal_start = p + usize::from(escaped);
            let literal = pattern[literal_start..].chars().next();
            let name_char = name[n..].chars().next().unwrap();
            if let Some(c) = literal.filter(|&c| (!escaped && c == '_') || c == name_char) {
                n += name_char.len_utf8();
                p = literal_start + c.len_utf8();
            } else if let Some(s) = star {
                retry += name[retry..].chars().next().unwrap().len_utf8();
                n = retry;
                p = s;
            } else {
                return false;
            }
        }
    }
    while pattern[p..].starts_with('%') { p += 1; }
    p == pattern.len()
}

#[cfg(all(test, feature = "tests-services"))]
mod tests;
