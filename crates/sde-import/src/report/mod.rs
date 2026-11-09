//! Shared keys and modules for deterministic offline import reports.
//! This reporting layer is not a client service or protocol mirror.
//! It compares source records and publishes manifest and difference evidence.
//! Report ordering and schemas do not define client or gameplay behavior.
pub mod diff;
pub mod manifest;
pub mod source;

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Integer keys are canonical signed decimal strings, never machine-sized integers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum ReportKey { Integer(String), String(String) }

impl ReportKey {
    pub fn integer(value: &str) -> Result<Self, String> {
        let (negative, digits) = match value.strip_prefix('-') { Some(s) => (true, s), None => (false, value) };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { return Err("invalid integer key".into()); }
        let digits = digits.trim_start_matches('0');
        Ok(Self::Integer(if digits.is_empty() { "0".into() } else if negative { format!("-{digits}") } else { digits.into() }))
    }
}
impl Ord for ReportKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => {
                let an = a.starts_with('-'); let bn = b.starts_with('-');
                match (an, bn) {
                    (true, false) => Ordering::Less, (false, true) => Ordering::Greater,
                    _ => { let a = a.trim_start_matches('-'); let b = b.trim_start_matches('-');
                        let order = a.len().cmp(&b.len()).then_with(|| a.cmp(b));
                        if an { order.reverse() } else { order }
                    }
                }
            }
            (Self::Integer(_), Self::String(_)) => Ordering::Less,
            (Self::String(_), Self::Integer(_)) => Ordering::Greater,
            (Self::String(a), Self::String(b)) => a.cmp(b),
        }
    }
}
impl PartialOrd for ReportKey { fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) } }
