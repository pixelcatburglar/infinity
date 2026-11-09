//! Search-word moderation using the cached retail `bannedwords.fetch_regexes` results.
//! Mirrors `bannedwords/bannedwords_regexes.py` and its marker stripper; no external
//! moderation service, live rule updates or Resource Wars lifecycle is implemented.

use std::sync::LazyLock;

use fancy_regex::Regex;
use marshal::Value;
use tracing::warn;

use super::{user_error, Call, CallResult};
use infinity_core::messages::{MSG_BANNED_WORDS_DOWN, MSG_LOCATION_NAME_INVALID_BANNED_WORD};

/// Client `bannedwords_regexes.MAX_CHECK_LENGTH`, in Unicode characters.
const MAX_CHECK_LENGTH: usize = 200;
/// Client `bannedwords_regexes.OVERLAPPING_CHOP`.
const OVERLAPPING_CHOP: usize = 20;

struct Rules {
    patterns: Vec<Regex>,
    markers: Vec<(Regex, Vec<Regex>)>,
}

// Python 2's default shorthand character classes are ASCII, including in sets.
fn translate(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    let mut class = false;
    let mut first = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let Some(escaped) = chars.next() else { out.push(ch); break };
            let replacement = match (escaped, class) {
                ('d', _) => Some("[0-9]"),
                ('D', _) => Some("[^0-9]"),
                ('s', _) => Some(r"[\x09-\x0d\x20]"),
                ('S', _) => Some(r"[^\x09-\x0d\x20]"),
                ('w', _) => Some("[a-zA-Z0-9_]"),
                ('W', _) => Some("[^a-zA-Z0-9_]"),
                ('b', true) => Some(r"\x08"),
                ('b', false) => Some(r"(?:(?<=[a-zA-Z0-9_])(?![a-zA-Z0-9_])|(?<![a-zA-Z0-9_])(?=[a-zA-Z0-9_]))"),
                ('B', false) => Some(r"(?!\A\z)(?:(?<=[a-zA-Z0-9_])(?=[a-zA-Z0-9_])|(?<![a-zA-Z0-9_])(?![a-zA-Z0-9_]))"),
                _ => None,
            };
            if let Some(replacement) = replacement { out.push_str(replacement); }
            else { out.push('\\'); out.push(escaped); }
            first = false;
        } else {
            if ch == '[' && !class { class = true; first = true; }
            else if class && ch == ']' && !first { class = false; }
            else if class && !(first && ch == '^') { first = false; }
            out.push(ch);
        }
    }
    out
}

fn compile_payloads(payloads: &[Value]) -> Result<Vec<Regex>, String> {
    let mut patterns = Vec::new();
    for (source, payload) in payloads.iter().enumerate() {
        let entries = payload.items().ok_or_else(|| format!("rule source {source} is not a list"))?;
        if entries.is_empty() { return Err(format!("rule source {source} is empty")); }
        for (index, entry) in entries.iter().enumerate() {
            let text = match entry.get("regex") {
                Some(Value::Unicode(text)) => text.as_str(),
                Some(Value::Bytes(bytes)) => std::str::from_utf8(bytes)
                    .map_err(|_| format!("rule source {source} index {index} is not UTF-8"))?,
                _ => return Err(format!("rule source {source} index {index} has no text")),
            };
            let rule = Regex::new(&translate(text))
                .map_err(|error| format!("rule source {source} index {index}: {error}"))?;
            patterns.push(rule);
        }
    }
    if patterns.is_empty() { return Err("no moderation rules".into()); }
    Ok(patterns)
}

fn load_rules() -> Result<Rules, String> {
    let mut payloads = Vec::new();
    for kind in ["blocked", "intercepted"] {
        let value = crate::retail::cached_call("bannedwords", "fetch_regexes", &[Value::str(kind)])
            .ok_or_else(|| format!("missing cached {kind} rules"))?;
        payloads.push(value);
    }
    let patterns = compile_payloads(&payloads)?;
    // Marker names and valid attributes follow the client's formatting stripper.
    let definitions: &[(&str, &[&str])] = &[
        ("font", &[r#"size=?"?[0-9]{1,2}"?"#, r#"color=?"?(#|0x)[0-9abcdef]{6,8}"?"#]),
        ("a", &[r#"href=?"?(fitting|joinchannel|overviewpreset|showinfo|killreport):[0-9abcdef/;:\-]+"?"#]),
        ("color", &[r#"=?"?(#|0x)[0-9abcdef]{6,8}"?"#]),
        ("fontsize", &[r#"=?"?[0-9]{1,2}"?"#]),
        ("u", &[]), ("i", &[]), ("b", &[]), ("loc", &[]),
        ("url", &[r#"=?"?(fitting|joinchannel|overviewpreset|showinfo|killreport):[0-9abcdef/;:\-]+"?"#]),
    ];
    let mut markers = Vec::new();
    for (tag, attributes) in definitions {
        let tag_pattern = Regex::new(&format!(r"(?i)<{tag}([^>]*)>(.*?)</{tag}>")).map_err(|e| e.to_string())?;
        let attrs = attributes.iter().map(|s| Regex::new(&format!("(?i){s}")).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        markers.push((tag_pattern, attrs));
    }
    Ok(Rules { patterns, markers })
}

fn strip_markers(text: &str, markers: &[(Regex, Vec<Regex>)]) -> Result<String, fancy_regex::Error> {
    let mut text = text.replace("<br>", "");
    for (pattern, attributes) in markers {
        let mut result = String::new();
        let mut last = 0;
        for captures in pattern.captures_iter(&text) {
            let captures = captures?;
            let Some(full) = captures.get(0) else { continue };
            result.push_str(&text[last..full.start()]);
            let mut attrs = captures.get(1).map_or("", |m| m.as_str()).to_owned();
            for attribute in attributes {
                attrs = attribute.replace_all(&attrs, "").into_owned();
            }
            result.push_str(attrs.trim());
            result.push_str(captures.get(2).map_or("", |m| m.as_str()));
            last = full.end();
        }
        if last != 0 {
            result.push_str(&text[last..]);
            text = result;
        }
    }
    Ok(text)
}

fn check_text(text: &str, rules: &Rules) -> Result<bool, String> {
    if text.is_empty() { return Ok(false); }
    let text = strip_markers(text, &rules.markers).map_err(|error| format!("marker stripping: {error}"))?;
    let mut offsets: Vec<usize> = text.char_indices().map(|(offset, _)| offset).collect();
    offsets.push(text.len());
    let length = offsets.len() - 1;
    let step = MAX_CHECK_LENGTH - OVERLAPPING_CHOP;
    for start in (0..length.max(1)).step_by(step) {
        let end = (start + MAX_CHECK_LENGTH).min(length);
        let slice = &text[offsets[start]..offsets[end]];
        for (index, pattern) in rules.patterns.iter().enumerate() {
            match pattern.is_match(slice) {
                Ok(true) => return Ok(true),
                Ok(false) => {},
                Err(error) => return Err(format!("rule index {index}: {error}")),
            }
        }
        if length <= MAX_CHECK_LENGTH { break; }
    }
    Ok(false)
}

fn check(call: &Call<'_>, rules: Result<&Rules, &String>) -> CallResult {
    let error_message = call.kwargs.get("err_message").and_then(Value::as_text)
        .map(|s| s.into_owned()).unwrap_or_else(|| MSG_LOCATION_NAME_INVALID_BANNED_WORD.to_owned());
    for arg in call.args {
        if matches!(arg, Value::None | Value::Bool(false) | Value::Int(0)) { continue; }
        if matches!(arg, Value::Bytes(bytes) if std::str::from_utf8(bytes).is_err()) {
            return Err(user_error(MSG_BANNED_WORDS_DOWN, vec![]));
        }
        let text = match arg {
            Value::Int(n) => n.to_string(),
            Value::Float(n) if *n == 0.0 => continue,
            Value::Float(n) => n.to_string(),
            _ => match arg.as_text() {
                Some(text) => text.into_owned(),
                None => return Err(user_error(MSG_BANNED_WORDS_DOWN, vec![])),
            },
        };
        if text.is_empty() { continue; }
        let rules = match rules {
            Ok(rules) => rules,
            Err(error) => { warn!("search moderation unavailable: {error}"); return Err(user_error(MSG_BANNED_WORDS_DOWN, vec![])); }
        };
        match check_text(&text, rules) {
            Ok(true) => return Err(user_error(&error_message, vec![])),
            Ok(false) => {},
            Err(error) => { warn!("search moderation failed: {error}"); return Err(user_error(&error_message, vec![])); }
        }
    }
    Ok(Value::None)
}

pub(super) fn dispatch(call: &Call<'_>) -> Option<CallResult> {
    if (call.service, call.method) != ("bannedwords", "check_search_words_allowed") { return None; }
    static RULES: LazyLock<Result<Rules, String>> = LazyLock::new(load_rules);
    Some(check(call, RULES.as_ref()))
}

#[cfg(test)] mod tests;
