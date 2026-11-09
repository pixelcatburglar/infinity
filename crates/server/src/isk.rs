//! Exact ISK amounts used by wallet authority and journal entries.
//! Client and Lua numeric projections are lossy; f64 projections are quote-only,
//! not authoritative arithmetic. Database and event values are decimal.
//! The domain is the signed PostgreSQL numeric(36,2) coefficient.

use std::{fmt, str::FromStr};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use infinity_core::consts::{CLIENT_MAX_CORP_ISK, CLIENT_MAX_ISK};
use std::sync::LazyLock;
/// The coefficient limit of a numeric(36,2) wallet value.
const MAX_CENTS: i128 = 999_999_999_999_999_999_999_999_999_999_999_999;
/// Exclusive coefficient bound for PostgreSQL numeric(36,2), projected to f64.
const CENTS_EXCLUSIVE_LIMIT: f64 = (MAX_CENTS + 1) as f64;
/// The coefficient limit of a numeric(37,2) journal snapshot.
pub(crate) const MAX_JOURNAL_CENTS: i128 = 9_999_999_999_999_999_999_999_999_999_999_999_999;
/// ISK's two-decimal currency quantum: one ISK is one hundred cents.
const CENTS_PER_ISK: i128 = 100;
/// Protobuf Currency represents one whole unit as one billion nanos.
const NANOS_PER_UNIT: i128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct IskAmount(i128);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmountError;
impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("invalid or out-of-range ISK amount") }
}
impl std::error::Error for AmountError {}

/// Parse an exact two-decimal coefficient under the caller's domain limit.
pub(crate) fn parse_decimal_coefficient(value: &str, limit: i128) -> Result<i128, AmountError> {
    let bytes = value.as_bytes();
    let (negative, start) = match bytes.first() {
        Some(b'-') => (true, 1),
        Some(b'+') => (false, 1),
        _ => (false, 0),
    };
    let mut cents = 0i128;
    let mut whole_digits = 0usize;
    let mut fractional_digits = None;
    for &byte in &bytes[start..] {
        if byte == b'.' && fractional_digits.is_none() && whole_digits != 0 {
            fractional_digits = Some(0usize);
        } else if byte.is_ascii_digit() {
            if let Some(count) = &mut fractional_digits {
                if *count < 2 {
                    cents = cents.checked_mul(10).and_then(|n| n.checked_add(i128::from(byte - b'0'))).ok_or(AmountError)?;
                } else if byte != b'0' {
                    return Err(AmountError);
                }
                *count += 1;
            } else {
                cents = cents.checked_mul(10).and_then(|n| n.checked_add(i128::from(byte - b'0'))).ok_or(AmountError)?;
                whole_digits += 1;
            }
        } else {
            return Err(AmountError);
        }
    }
    if whole_digits == 0 || fractional_digits == Some(0) { return Err(AmountError); }
    for _ in fractional_digits.unwrap_or(0)..2 {
        cents = cents.checked_mul(10).ok_or(AmountError)?;
    }
    if cents > limit { return Err(AmountError); }
    Ok(if negative { -cents } else { cents })
}

// Four little-endian limbs hold two numeric(36,2) coefficients multiplied by
// u128 weights and added together without losing cancellation.
fn weighted_product(amount: u128, weight: u128) -> [u64; 4] {
    let a = [amount as u64, (amount >> u64::BITS) as u64];
    let b = [weight as u64, (weight >> u64::BITS) as u64];
    let mut result = [0_u64; 4];
    for i in 0..2 {
        let mut carry = 0_u128;
        for j in 0..2 {
            let value = u128::from(a[i]) * u128::from(b[j])
                + u128::from(result[i + j]) + carry;
            result[i + j] = value as u64;
            carry = value >> u64::BITS;
        }
        result[i + 2] = carry as u64;
    }
    result
}

fn rounded_wide_amount(magnitude: [u64; 4], negative: bool, denominator: u128) -> Option<IskAmount> {
    if denominator == 0 { return None; }
    let mut quotient = 0_u128;
    let mut remainder = 0_u128;
    for bit in (0..u64::BITS as usize * magnitude.len()).rev() {
        quotient = quotient.checked_mul(2)?;
        if remainder >= denominator - remainder {
            remainder -= denominator - remainder;
            quotient = quotient.checked_add(1)?;
        } else {
            remainder *= 2;
        }
        if (magnitude[bit / u64::BITS as usize] >> (bit % u64::BITS as usize)) & 1 != 0 {
            if remainder == denominator - 1 {
                remainder = 0;
                quotient = quotient.checked_add(1)?;
            } else {
                remainder += 1;
            }
        }
        if quotient > MAX_CENTS as u128 { return None; }
    }
    if remainder >= denominator - remainder { quotient = quotient.checked_add(1)?; }
    if quotient > MAX_CENTS as u128 { return None; }
    let cents = quotient as i128;
    IskAmount::from_cents(if negative { -cents } else { cents })
}

impl IskAmount {
    pub(crate) const ZERO: Self = Self(0);
    pub(crate) fn from_cents(cents: i128) -> Option<Self> {
        if (-MAX_CENTS..=MAX_CENTS).contains(&cents) { Some(Self(cents)) } else { None }
    }
    pub(crate) fn from_whole_isk(isk: i128) -> Option<Self> {
        isk.checked_mul(CENTS_PER_ISK).and_then(Self::from_cents)
    }
    pub(crate) fn parse_decimal(value: &str) -> Result<Self, AmountError> {
        parse_decimal_coefficient(value, MAX_CENTS).map(Self)
    }
    pub(crate) fn from_isk(value: f64) -> Result<Self, AmountError> {
        if !value.is_finite() { return Err(AmountError); }
        let cents = (value * CENTS_PER_ISK as f64).round();
        if !cents.is_finite() || cents.abs() >= CENTS_EXCLUSIVE_LIMIT { return Err(AmountError); }
        Self::from_cents(cents as i128).ok_or(AmountError)
    }
    pub(crate) fn checked_add(self, rhs: Self) -> Option<Self> { self.0.checked_add(rhs.0).and_then(Self::from_cents) }
    pub(crate) fn checked_mul(self, quantity: i64) -> Option<Self> { self.0.checked_mul(i128::from(quantity)).and_then(Self::from_cents) }
    /// Multiply cents by an unsigned ratio, rounding half cents away from zero.
    pub(crate) fn checked_mul_ratio(self, numerator: u128, denominator: u128) -> Option<Self> {
        self.checked_weighted_sum(numerator, Self::ZERO, 0, denominator)
    }
    /// Sum two signed weighted amounts exactly, then round once to nearest cent,
    /// with ties away from zero. Returns None for zero denominator or overflow.
    pub(crate) fn checked_weighted_sum(
        self, self_weight: u128, other: Self, other_weight: u128, denominator: u128,
    ) -> Option<Self> {
        let mut left = weighted_product(self.0.unsigned_abs(), self_weight);
        let mut right = weighted_product(other.0.unsigned_abs(), other_weight);
        let negative;
        if self.is_negative() == other.is_negative() {
            negative = self.is_negative();
            let mut carry = false;
            for i in 0..left.len() {
                let (sum, c1) = left[i].overflowing_add(right[i]);
                let (sum, c2) = sum.overflowing_add(u64::from(carry));
                left[i] = sum;
                carry = c1 || c2;
            }
            debug_assert!(!carry); // The two bounded products fit in 256 bits.
        } else {
            let right_larger = left.iter().rev().cmp(right.iter().rev()).is_lt();
            if right_larger { std::mem::swap(&mut left, &mut right); }
            negative = if right_larger { other.is_negative() } else { self.is_negative() };
            let mut borrow = false;
            for i in 0..left.len() {
                let (difference, b1) = left[i].overflowing_sub(right[i]);
                let (difference, b2) = difference.overflowing_sub(u64::from(borrow));
                left[i] = difference;
                borrow = b1 || b2;
            }
        }
        rounded_wide_amount(left, negative, denominator)
    }
    pub(crate) fn checked_sub(self, rhs: Self) -> Option<Self> { self.0.checked_sub(rhs.0).and_then(Self::from_cents) }
    pub(crate) fn checked_neg(self) -> Option<Self> { self.0.checked_neg().and_then(Self::from_cents) }
    pub(crate) fn is_negative(self) -> bool { self.0 < 0 }
    pub(crate) fn is_zero(self) -> bool { self.0 == 0 }
    /// Project exact cents to normalized signed protobuf Currency units and nanos.
    /// Returns None when the whole units do not fit the wire's signed int64.
    pub(crate) fn split_precision(self) -> Option<(i64, i32)> {
        let units = i64::try_from(self.0 / CENTS_PER_ISK).ok()?;
        let nanos = ((self.0 % CENTS_PER_ISK) * (NANOS_PER_UNIT / CENTS_PER_ISK)) as i32;
        Some((units, nanos))
    }
    pub(crate) fn decimal(self) -> String {
        let magnitude = self.0.abs();
        format!("{}{}.{:02}", if self.is_negative() { "-" } else { "" }, magnitude / CENTS_PER_ISK, magnitude % CENTS_PER_ISK)
    }
    pub(crate) fn wire_isk(self) -> f64 { self.0 as f64 / CENTS_PER_ISK as f64 }
    pub(crate) fn client_isk(self, cap: Self) -> f64 { Self::client_cents(self.0, cap) }
    pub(crate) fn client_cents(cents: i128, cap: Self) -> f64 { cents.clamp(-cap.0, cap.0) as f64 / CENTS_PER_ISK as f64 }
}
impl FromStr for IskAmount {
    type Err = AmountError;
    fn from_str(s: &str) -> Result<Self, Self::Err> { Self::parse_decimal(s) }
}
impl Serialize for IskAmount {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> { serializer.serialize_str(&self.decimal()) }
}
impl<'de> Deserialize<'de> for IskAmount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse_decimal(&value).map_err(serde::de::Error::custom)
    }
}
impl mlua::IntoLua for IskAmount {
    fn into_lua(self, _: &mlua::Lua) -> mlua::Result<mlua::Value> { Ok(mlua::Value::Number(self.wire_isk())) }
}

#[cfg(test)]
mod tests;
static PERSONAL_CAP: LazyLock<IskAmount> = LazyLock::new(|| IskAmount::from_whole_isk(CLIENT_MAX_ISK as i128).expect("valid personal client cap"));
static CORPORATION_CAP: LazyLock<IskAmount> = LazyLock::new(|| IskAmount::from_whole_isk(CLIENT_MAX_CORP_ISK as i128).expect("valid corporation client cap"));
pub(crate) fn personal_client_cap() -> IskAmount { *PERSONAL_CAP }
pub(crate) fn corporation_client_cap() -> IskAmount { *CORPORATION_CAP }
