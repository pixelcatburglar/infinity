//! Parse exact JSON numbers and report recursive structural differences.
//! This offline comparison is not a client service or protocol mirror.
//! It compares supplied records with ordered arrays and decimal number values.
//! Input size, nesting and numeric budgets are bounded; it does not execute gameplay.
use super::ReportKey;
use serde::{Serialize, ser::{SerializeMap, SerializeSeq, Serializer}};
use serde_json::value::RawValue;
use std::collections::BTreeMap;

/// Limits apply before allocating or recursively descending into untrusted input.
pub const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_JSON_DEPTH: usize = 128;
pub const MAX_NUMBER_BYTES: usize = 1024 * 1024;
/// PostgreSQL numeric storage limits: https://www.postgresql.org/docs/current/datatype-numeric.html
const PG_NUMERIC_INTEGER_DIGITS: i128 = 131_072;
const PG_NUMERIC_FRACTION_DIGITS: i128 = 16_383;
/// PostgreSQL 18 numeric.c set_var_from_str limits the exponent to PG_INT32_MAX / 2:
/// https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/utils/adt/numeric.c
const PG_NUMERIC_MAX_EXPONENT: i128 = 1_073_741_823;

#[derive(Clone, Debug)]
pub struct ExactNumber { pub token: String, pub negative_zero: bool, negative: bool, coefficient: String, exponent: i64 }
impl PartialEq for ExactNumber {
    fn eq(&self, other: &Self) -> bool { self.negative == other.negative && self.coefficient == other.coefficient && self.exponent == other.exponent }
}
impl Eq for ExactNumber {}
impl ExactNumber {
    pub fn parse(token: &str) -> Result<Self, String> {
        if token.len() > MAX_NUMBER_BYTES || token.is_empty() { return Err("number exceeds budget or is empty".into()); }
        let bytes = token.as_bytes(); let mut i = 0;
        let negative = bytes.first() == Some(&b'-'); if negative { i += 1; }
        let start = i;
        if bytes.get(i) == Some(&b'0') { i += 1; if bytes.get(i).is_some_and(u8::is_ascii_digit) { return Err("leading zero".into()); } }
        else { while bytes.get(i).is_some_and(u8::is_ascii_digit) { i += 1; } if start == i { return Err("missing integer digits".into()); } }
        let int_end = i; let mut fraction_start = i; let mut fraction_end = i;
        if bytes.get(i) == Some(&b'.') { i += 1; fraction_start = i; while bytes.get(i).is_some_and(u8::is_ascii_digit) { i += 1; } fraction_end = i; if fraction_start == i { return Err("missing fraction digits".into()); } }
        let mut exponent = 0i64;
        if matches!(bytes.get(i), Some(b'e' | b'E')) {
            i += 1; let exp_negative = bytes.get(i) == Some(&b'-');
            if matches!(bytes.get(i), Some(b'+' | b'-')) { i += 1; }
            let exp_start = i;
            while let Some(b) = bytes.get(i).filter(|b| b.is_ascii_digit()) { exponent = exponent.checked_mul(10).and_then(|v| v.checked_add(i64::from(*b - b'0'))).ok_or("exponent overflow")?; i += 1; }
            if exp_start == i { return Err("missing exponent digits".into()); }
            if exp_negative { exponent = exponent.checked_neg().ok_or("exponent overflow")?; }
        }
        if i != bytes.len() { return Err("invalid number suffix".into()); }
        exponent = exponent.checked_sub(i64::try_from(fraction_end - fraction_start).map_err(|_| "fraction overflow")?).ok_or("scale overflow")?;
        let mut coefficient = String::with_capacity(int_end - start + fraction_end - fraction_start);
        coefficient.push_str(&token[start..int_end]); coefficient.push_str(&token[fraction_start..fraction_end]);
        let trimmed = coefficient.trim_start_matches('0');
        if trimmed.is_empty() { return Ok(Self { token: token.into(), negative_zero: negative, negative: false, coefficient: "0".into(), exponent: 0 }); }
        let trailing = trimmed.len() - trimmed.trim_end_matches('0').len();
        exponent = exponent.checked_add(i64::try_from(trailing).map_err(|_| "scale overflow")?).ok_or("scale overflow")?;
        Ok(Self { token: token.into(), negative_zero: false, negative, coefficient: trimmed[..trimmed.len()-trailing].into(), exponent })
    }
    /// Whether the canonical decimal value is zero, independent of its spelling or sign.
    pub fn is_zero(&self) -> bool { self.coefficient == "0" }
    /// Check JSONB's numeric storage domain without changing the source token.
    pub fn fits_postgres_numeric(&self) -> bool {
        let (mantissa, exponent) = self.token.split_once(['e', 'E']).map_or((self.token.as_str(), "0"), |parts| parts);
        let exponent: i128 = exponent.parse().expect("validated decimal exponent");
        let scale = mantissa.split_once('.').map_or(0, |(_, fraction)| fraction.len()) as i128 - exponent;
        let integer_digits = self.coefficient.len() as i128 + i128::from(self.exponent);
        exponent <= PG_NUMERIC_MAX_EXPONENT && scale <= PG_NUMERIC_FRACTION_DIGITS
            && (self.is_zero() || integer_digits <= PG_NUMERIC_INTEGER_DIGITS)
    }
    pub fn finite_f64(&self) -> Result<f64, String> {
        let n: f64 = self.token.parse().map_err(|_| "invalid f64")?;
        if n.is_finite() { Ok(n) } else { Err("nonfinite f64".into()) }
    }
    /// PostgreSQL numeric-to-bigint rounding: nearest integer, ties away from zero.
    pub fn as_i64_rounded(&self) -> Result<i64, String> {
        let digits = self.coefficient.as_bytes();
        let (integer_digits, zeroes, round_up) = if self.exponent >= 0 {
            let zeroes = usize::try_from(self.exponent).map_err(|_| "integer overflow")?;
            if digits.len().checked_add(zeroes).is_none_or(|n| n > 19) { return Err("integer overflow".into()); }
            (digits.len(), zeroes, false)
        } else {
            let scale = self.exponent.unsigned_abs();
            if scale > digits.len() as u64 { (0, 0, false) }
            else {
                let integer_digits = digits.len() - scale as usize;
                if integer_digits > 19 { return Err("integer overflow".into()); }
                (integer_digits, 0, digits[integer_digits] >= b'5')
            }
        };
        let mut magnitude = 0i128;
        for &digit in &digits[..integer_digits] { magnitude = magnitude * 10 + i128::from(digit - b'0'); }
        for _ in 0..zeroes { magnitude *= 10; }
        if round_up { magnitude += 1; }
        let signed = if self.negative { -magnitude } else { magnitude };
        i64::try_from(signed).map_err(|_| "integer overflow".into())
    }
    /// PostgreSQL numeric-to-integer rounding, with a checked i32 result.
    pub fn as_i32_rounded(&self) -> Result<i32, String> {
        i32::try_from(self.as_i64_rounded()?).map_err(|_| "integer overflow".into())
    }
    /// JSONB renders numeric exponents as decimal text, retaining fractional scale.
    /// PostgreSQL integer text casts reject a decimal point rather than rounding.
    pub fn as_jsonb_text_i64(&self) -> Result<i64, String> {
        let (mantissa, exponent) = self.token.split_once(['e', 'E']).map_or((self.token.as_str(), "0"), |parts| parts);
        let exponent: i64 = exponent.parse().map_err(|_| "exponent overflow")?;
        let fraction = mantissa.split_once('.').map_or(0, |(_, digits)| digits.len());
        if i64::try_from(fraction).map_err(|_| "scale overflow")? > exponent {
            return Err("integer text contains decimal point".into());
        }
        self.as_i64_rounded()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExactValue { Null, Bool(bool), Number(ExactNumber), String(String), Array(Vec<ExactValue>), Object(BTreeMap<String, ExactValue>) }
impl Serialize for ExactValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(), Self::Bool(v) => serializer.serialize_bool(*v),
            Self::Number(n) => RawValue::from_string(n.token.clone()).map_err(serde::ser::Error::custom)?.serialize(serializer),
            Self::String(s) => serializer.serialize_str(s),
            Self::Array(a) => { let mut seq = serializer.serialize_seq(Some(a.len()))?; for v in a { seq.serialize_element(v)?; } seq.end() },
            Self::Object(o) => { let mut map = serializer.serialize_map(Some(o.len()))?; for (k,v) in o { map.serialize_entry(k,v)?; } map.end() }
        }
    }
}

struct Parser<'a> { text: &'a str, pos: usize }
impl<'a> Parser<'a> {
    fn ws(&mut self) { while self.text.as_bytes().get(self.pos).is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t')) { self.pos += 1; } }
    fn string(&mut self) -> Result<String, String> {
        let start = self.pos; self.pos += 1; let b = self.text.as_bytes();
        while let Some(&c) = b.get(self.pos) { self.pos += 1; if c == b'\\' { if self.pos >= b.len() { return Err("truncated escape".into()); } self.pos += 1; } else if c == b'"' { return serde_json::from_str(&self.text[start..self.pos]).map_err(|e| e.to_string()); } }
        Err("unterminated string".into())
    }
    fn value(&mut self, depth: usize) -> Result<ExactValue, String> {
        if depth > MAX_JSON_DEPTH { return Err("JSON depth budget exceeded".into()); } self.ws();
        let text = self.text; let b = text.as_bytes();
        match b.get(self.pos).copied() {
            Some(b'"') => Ok(ExactValue::String(self.string()?)),
            Some(b'[') => { self.pos += 1; let mut v = Vec::new(); self.ws(); if b.get(self.pos) == Some(&b']') { self.pos += 1; return Ok(ExactValue::Array(v)); }
                loop { v.push(self.value(depth+1)?); self.ws(); match b.get(self.pos) { Some(b',') => self.pos += 1, Some(b']') => { self.pos += 1; break; }, _ => return Err("expected array delimiter".into()) } } Ok(ExactValue::Array(v)) },
            Some(b'{') => { self.pos += 1; let mut v = BTreeMap::new(); self.ws(); if b.get(self.pos) == Some(&b'}') { self.pos += 1; return Ok(ExactValue::Object(v)); }
                loop { self.ws(); if b.get(self.pos) != Some(&b'"') { return Err("expected object key".into()); } let key = self.string()?; self.ws(); if b.get(self.pos) != Some(&b':') { return Err("expected colon".into()); } self.pos += 1; let value = self.value(depth+1)?; if v.insert(key, value).is_some() { return Err("duplicate object key".into()); } self.ws(); match b.get(self.pos) { Some(b',') => self.pos += 1, Some(b'}') => { self.pos += 1; break; }, _ => return Err("expected object delimiter".into()) } } Ok(ExactValue::Object(v)) },
            Some(b't') if self.text[self.pos..].starts_with("true") => { self.pos += 4; Ok(ExactValue::Bool(true)) },
            Some(b'f') if self.text[self.pos..].starts_with("false") => { self.pos += 5; Ok(ExactValue::Bool(false)) },
            Some(b'n') if self.text[self.pos..].starts_with("null") => { self.pos += 4; Ok(ExactValue::Null) },
            Some(b'-' | b'0'..=b'9') => { let start = self.pos; while b.get(self.pos).is_some_and(|c| matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')) { self.pos += 1; } Ok(ExactValue::Number(ExactNumber::parse(&self.text[start..self.pos])?)) },
            _ => Err("expected JSON value".into()),
        }
    }
}
pub fn parse_json(text: &str) -> Result<ExactValue, String> {
    if text.len() > MAX_JSON_BYTES { return Err("JSON byte budget exceeded".into()); }
    let mut parser = Parser { text, pos: 0 }; let value = parser.value(0)?; parser.ws();
    if parser.pos != text.len() { return Err("trailing JSON data".into()); } Ok(value)
}

#[derive(Clone, Debug, Serialize)]
pub struct Difference {
    pub family: String, pub key: ReportKey, pub path: String, pub kind: String,
    pub old_present: bool, pub new_present: bool, pub old_value: Option<ExactValue>, pub new_value: Option<ExactValue>,
    pub numeric_classification: String, pub source_relative_path: String, pub source_sha256: String,
}
fn pointer(path: &str, part: &str) -> String { format!("{path}/{}", part.replace('~', "~0").replace('/', "~1")) }
/// Returns all record or leaf changes; exact values retain their original numeric spelling.
pub fn compare(family: &str, key: ReportKey, old: Option<&ExactValue>, new: Option<&ExactValue>, source_relative_path: &str, source_sha256: &str) -> Vec<Difference> {
    fn visit(out: &mut Vec<Difference>, template: &Difference, path: &str, old: Option<&ExactValue>, new: Option<&ExactValue>, root: bool) {
        if old == new { return; }
        match (old, new) {
            (Some(ExactValue::Object(a)), Some(ExactValue::Object(b))) => {
                let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
                for k in keys { visit(out, template, &pointer(path, k), a.get(k), b.get(k), false); } return;
            }
            (Some(ExactValue::Array(a)), Some(ExactValue::Array(b))) => {
                for i in 0..a.len().max(b.len()) { visit(out, template, &pointer(path, &i.to_string()), a.get(i), b.get(i), false); } return;
            }
            _ => {}
        }
        let mut d = template.clone(); d.path = path.into(); d.old_present = old.is_some(); d.new_present = new.is_some(); d.old_value = old.cloned(); d.new_value = new.cloned();
        d.kind = if root && old.is_none() { "client-only" } else if root && new.is_none() { "sde-only" } else { "changed" }.into();
        if let (Some(ExactValue::Number(a)), Some(ExactValue::Number(b))) = (old, new) {
            d.numeric_classification = if matches!((a.finite_f64(), b.finite_f64()), (Ok(x), Ok(y)) if x.to_bits() == y.to_bits()) { "f64-representation-only" } else { "exact-change" }.into();
        }
        out.push(d);
    }
    let template = Difference { family: family.into(), key, path: String::new(), kind: String::new(), old_present: false, new_present: false, old_value: None, new_value: None, numeric_classification: "not-numeric".into(), source_relative_path: source_relative_path.into(), source_sha256: source_sha256.into() };
    let mut out = Vec::new(); visit(&mut out, &template, "", old, new, true); out.sort_by(|a,b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind))); out
}

#[cfg(test)] mod tests;
