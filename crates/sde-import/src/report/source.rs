//! Compare source records using exact expansions of known IEEE binary fields.
//! This offline comparison is not a client service or protocol mirror.
//! Types, meta-groups, dogma attributes and type dogma have designated binary paths;
//! snapshot integers remain decimal, while fraction/exponent tokens use f64 semantics.
//! COPY and raw JSONB readback retain their separate decimal domain; JSONB may
//! already have erased an exponent's original lexical provenance.
//! Other fields use ordinary exact JSON comparison, not inferred binary semantics.
use super::{diff::{self, Difference, ExactNumber, ExactValue}, ReportKey};
use std::collections::BTreeMap;

/// Proven resource-family provenance removed only from the old comparison view.
pub struct LanguageProjection {
    pub expected_language: &'static str,
    pub validated_rows: usize,
}

pub fn project_snapshot_language(family: &str, rows: &mut BTreeMap<ReportKey, ExactValue>) -> Result<Option<LanguageProjection>, String> {
    let expected = match family {
        "localization_fsd_en_us" => "en-us",
        "localization_fsd_zh" => "zh",
        _ => return Ok(None),
    };
    for (key, value) in rows.iter() {
        let valid = matches!(value, ExactValue::Object(fields)
            if matches!(fields.get("languageID"), Some(ExactValue::String(language)) if language == expected));
        if !valid {
            return Err(format!("{family} key {key:?} /languageID: expected string {expected:?}"));
        }
    }
    let validated_rows = rows.len();
    for value in rows.values_mut() {
        if let ExactValue::Object(fields) = value { fields.remove("languageID"); }
    }
    Ok(Some(LanguageProjection { expected_language: expected, validated_rows }))
}

/// Binary source provenance, expressed as paths within a native family record.
fn binary_path(family: &str, path: &[String]) -> bool {
    match (family, path) {
        ("types", [field]) => matches!(field.as_str(), "basePrice" | "capacity" | "mass" | "radius" | "volume"), // f64
        ("metagroups", [field, _]) => field == "color", // f32, promoted exactly to f64
        ("dogmaattributes", [field]) => field == "defaultValue", // f32, promoted exactly to f64
        ("typedogma", [field, _, value]) => field == "dogmaAttributes" && value == "value", // f64
        _ => false,
    }
}

/// Exact finite f64 as coefficient times a power of ten, using decimal digits
/// rather than an external bigint. The shortest JSON token is only used to
/// recover the original binary bits, never as the source equality value.
fn ieee_decimal(value: f64) -> Result<ExactNumber, String> {
    if !value.is_finite() { return Err("nonfinite source number".into()); }
    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, power) = if exponent == 0 { (fraction, -1074) } else { (fraction | (1u64 << 52), exponent - 1023 - 52) };
    if mantissa == 0 { return ExactNumber::parse(if negative { "-0" } else { "0" }); }
    let cancelled = mantissa.trailing_zeros().min(power.saturating_neg().max(0) as u32);
    let mantissa = mantissa >> cancelled;
    let power = power + cancelled as i32;
    let mut digits: Vec<u8> = mantissa.to_string().bytes().rev().map(|b| b - b'0').collect();
    let multiplier = if power < 0 { 5u8 } else { 2u8 };
    for _ in 0..power.unsigned_abs() {
        let mut carry = 0u8;
        for digit in &mut digits {
            let n = (*digit).checked_mul(multiplier).and_then(|n| n.checked_add(carry)).ok_or("decimal digit overflow")?;
            *digit = n % 10;
            carry = n / 10;
        }
        if carry != 0 { digits.push(carry); }
    }
    let coefficient: String = digits.iter().rev().map(|d| char::from(b'0' + *d)).collect();
    let token = format!("{}{}e{}", if negative { "-" } else { "" }, coefficient, power.min(0));
    ExactNumber::parse(&token)
}

fn transform(value: &mut ExactValue, family: &str, path: &mut Vec<String>, client: bool) -> Result<(), String> {
    if binary_path(family, path) {
        if let ExactValue::Number(number) = value {
            if client || number.token.bytes().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                *number = ieee_decimal(number.finite_f64()?)?;
            }
        }
        return Ok(());
    }
    match value {
        ExactValue::Object(fields) => for (key, child) in fields {
            path.push(key.clone()); transform(child, family, path, client)?; path.pop();
        },
        ExactValue::Array(items) => for (index, child) in items.iter_mut().enumerate() {
            path.push(index.to_string()); transform(child, family, path, client)?; path.pop();
        },
        _ => {}
    }
    Ok(())
}
// ExactValue equality deliberately normalizes zero for JSONB; source IEEE bits
// have an additional sign distinction that must be reported independently.
fn signed_zero_differences(out: &mut Vec<Difference>, template: &Difference, path: &mut Vec<String>, old: &ExactValue, new: &ExactValue) {
    if binary_path(&template.family, path) {
        if let (ExactValue::Number(a), ExactValue::Number(b)) = (old, new) {
            if a == b && a.negative_zero != b.negative_zero {
                let mut difference = template.clone();
                difference.path = path.iter().map(|part| format!("/{}", part.replace('~', "~0").replace('/', "~1"))).collect();
                difference.old_value = Some(old.clone());
                difference.new_value = Some(new.clone());
                out.push(difference);
            }
        }
        return;
    }
    match (old, new) {
        (ExactValue::Object(a), ExactValue::Object(b)) => for (key, left) in a {
            if let Some(right) = b.get(key) {
                path.push(key.clone()); signed_zero_differences(out, template, path, left, right); path.pop();
            }
        },
        (ExactValue::Array(a), ExactValue::Array(b)) => for (index, (left, right)) in a.iter().zip(b).enumerate() {
            path.push(index.to_string()); signed_zero_differences(out, template, path, left, right); path.pop();
        },
        _ => {}
    }
}


/// Compare source-domain values without modifying the prepared COPY/readback rows.
/// Reported binary-field values are exact decimal expansions, not COPY tokens.
pub fn compare(family: &str, key: ReportKey, old: Option<&ExactValue>, new: Option<&ExactValue>, source_relative_path: &str, source_sha256: &str) -> Result<Vec<Difference>, String> {
    if !matches!(family, "types" | "metagroups" | "dogmaattributes" | "typedogma") {
        return Ok(diff::compare(family, key, old, new, source_relative_path, source_sha256));
    }
    let mut old = old.cloned();
    let mut new = new.cloned();
    if let Some(value) = &mut old { transform(value, family, &mut Vec::new(), false)?; }
    if let Some(value) = &mut new { transform(value, family, &mut Vec::new(), true)?; }
    let template = Difference { family: family.into(), key: key.clone(), path: String::new(), kind: "changed".into(),
        old_present: true, new_present: true, old_value: None, new_value: None,
        numeric_classification: "exact-change".into(), source_relative_path: source_relative_path.into(), source_sha256: source_sha256.into() };
    let mut differences = diff::compare(family, key, old.as_ref(), new.as_ref(), source_relative_path, source_sha256);
    if let (Some(old), Some(new)) = (&old, &new) {
        signed_zero_differences(&mut differences, &template, &mut Vec::new(), old, new);
    }
    differences.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind)));
    Ok(differences)
}

#[cfg(test)] mod tests;
