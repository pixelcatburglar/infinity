//! Exact nonnegative sums of binary64 values in units of 2^-1074.
//! This is a server-owned representation, not a client protocol or currency type.
//! Capped progress retains the exact fractional binary remainder across batches.

use super::BinaryAmount;
// IEEE 754 binary64 has 53 significant bits and a minimum quantum of 2^-1074.
pub(super) const QUANTUM_EXPONENT: i16 = -1074;
pub(super) const FRACTION_BITS: u32 = f64::MANTISSA_DIGITS - 1;
const EXPONENT_MASK: u64 = (1 << (u64::BITS - FRACTION_BITS - 1)) - 1;
// A u64 count of maximum binary64 additions occupies at most 2162 bits.
pub(super) const MAX_SUM_BITS: usize = 2162;
pub(super) const LIMBS: usize = MAX_SUM_BITS.div_ceil(u64::BITS as usize);
pub(super) const MAX_EXPONENT: i16 = QUANTUM_EXPONENT + MAX_SUM_BITS as i16 - 1;
// ceil(log10(2^2162)); bounds a decimal coefficient before fixed-word parsing.
pub(super) const MAX_DECIMAL_DIGITS: usize = 651;
pub(super) const DECIMAL_RADIX: u128 = 10;
const WORD_BITS: usize = u64::BITS as usize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactBinarySum { limbs: [u64; LIMBS], occurrences: u64 }
impl Default for ExactBinarySum {
    fn default() -> Self { Self { limbs: [0; LIMBS], occurrences: 0 } }
}
impl ExactBinarySum {
    pub(crate) fn try_add(&mut self, value: f64) -> bool {
        if !value.is_finite() || value <= 0.0 { return false; }
        let Some(count) = self.occurrences.checked_add(1) else { return false; };
        let bits = value.to_bits();
        let exponent = ((bits >> FRACTION_BITS) & EXPONENT_MASK) as usize;
        let mantissa = (bits & ((1_u64 << FRACTION_BITS) - 1)) | if exponent == 0 { 0 } else { 1_u64 << FRACTION_BITS };
        let shift = exponent.saturating_sub(1);
        let mut limbs = self.limbs;
        let word = shift / WORD_BITS;
        let offset = shift % WORD_BITS;
        let mut carry = (mantissa as u128) << offset;
        let mut index = word;
        while carry != 0 {
            if index == limbs.len() { return false; }
            let sum = limbs[index] as u128 + (carry as u64) as u128;
            limbs[index] = sum as u64;
            carry = (carry >> WORD_BITS) + (sum >> WORD_BITS);
            index += 1;
        }
        self.limbs = limbs;
        self.occurrences = count;
        true
    }
    pub(crate) fn occurrences(&self) -> u64 { self.occurrences }
    pub(crate) fn to_amount(&self) -> BinaryAmount {
        let Some(first) = self.limbs.iter().position(|&word| word != 0) else {
            return BinaryAmount { coefficient: "0".into(), exponent: 0 };
        };
        let shift = first * WORD_BITS + self.limbs[first].trailing_zeros() as usize;
        let mut words = [0_u64; LIMBS];
        for (i, word) in words.iter_mut().enumerate().take(LIMBS - first) {
            *word = self.limbs[i + first] >> (shift % WORD_BITS);
            if shift % WORD_BITS != 0 && i + first + 1 < LIMBS {
                *word |= self.limbs[i + first + 1] << (WORD_BITS - shift % WORD_BITS);
            }
        }
        let mut digits = Vec::with_capacity(MAX_DECIMAL_DIGITS);
        while words.iter().any(|&word| word != 0) {
            let mut rem = 0_u128;
            for word in words.iter_mut().rev() {
                let n = (rem << WORD_BITS) | *word as u128;
                *word = (n / DECIMAL_RADIX) as u64;
                rem = n % DECIMAL_RADIX;
            }
            digits.push(b'0' + rem as u8);
        }
        digits.reverse();
        BinaryAmount { coefficient: String::from_utf8(digits).expect("decimal digits"), exponent: QUANTUM_EXPONENT + shift as i16 }
    }
}

// Parse and validate once into the same quantum-based carrier used by hit sums.
pub(super) fn parse(amount: &BinaryAmount) -> anyhow::Result<ExactBinarySum> {
    let s = amount.coefficient.as_bytes();
    anyhow::ensure!(!s.is_empty() && s.len() <= MAX_DECIMAL_DIGITS && s.iter().all(u8::is_ascii_digit), "invalid coefficient");
    if s == b"0" {
        anyhow::ensure!(amount.exponent == 0, "invalid zero");
        return Ok(ExactBinarySum::default());
    }
    anyhow::ensure!(s[0] != b'0' && (s[s.len()-1] - b'0') % 2 == 1, "noncanonical coefficient");
    anyhow::ensure!((QUANTUM_EXPONENT..=MAX_EXPONENT).contains(&amount.exponent), "invalid exponent");
    let mut words = [0_u64; LIMBS];
    for digit in s {
        let mut carry = (digit - b'0') as u128;
        for word in &mut words {
            let n = *word as u128 * DECIMAL_RADIX + carry;
            *word = n as u64;
            carry = n >> WORD_BITS;
        }
        anyhow::ensure!(carry == 0, "coefficient overflow");
    }
    let highest = words.iter().rposition(|&w| w != 0).unwrap();
    let bits = highest * WORD_BITS + (u64::BITS - words[highest].leading_zeros()) as usize;
    let shift = (amount.exponent - QUANTUM_EXPONENT) as usize;
    anyhow::ensure!(bits + shift <= MAX_SUM_BITS, "amount overflow");
    let mut sum = ExactBinarySum::default();
    let word_shift = shift / WORD_BITS;
    let offset = shift % WORD_BITS;
    for (i, &word) in words.iter().enumerate().take(highest + 1) {
        sum.limbs[i + word_shift] |= word << offset;
        if offset != 0 && i + word_shift + 1 < LIMBS {
            sum.limbs[i + word_shift + 1] |= word >> (WORD_BITS - offset);
        }
    }
    Ok(sum)
}
fn is_fraction(sum: &ExactBinarySum) -> bool {
    let bit = (-QUANTUM_EXPONENT) as usize;
    let word = bit / WORD_BITS;
    sum.limbs[word] >> (bit % WORD_BITS) == 0 && sum.limbs[word + 1..].iter().all(|&w| w == 0)
}

pub(crate) fn validate_remainder(remainder: &BinaryAmount) -> anyhow::Result<()> {
    anyhow::ensure!(is_fraction(&parse(remainder)?), "remainder must be less than one");
    Ok(())
}


/// Adds an exact nonnegative increment, retaining fractional progress until completion.
pub(crate) fn accumulate_capped(remainder: &BinaryAmount, increment: &BinaryAmount, remaining: u64) -> anyhow::Result<(u64, BinaryAmount)> {
    let fraction = parse(remainder)?;
    let mut sum = parse(increment)?;
    let integer_bit = (-QUANTUM_EXPONENT) as usize;
    let word = integer_bit / WORD_BITS;
    let offset = integer_bit % WORD_BITS;
    anyhow::ensure!(is_fraction(&fraction), "remainder must be less than one");
    let mut carry = 0_u128;
    for (dst, &src) in sum.limbs.iter_mut().zip(&fraction.limbs) {
        let value = *dst as u128 + src as u128 + carry;
        *dst = value as u64;
        carry = value >> WORD_BITS;
    }
    let integer = (sum.limbs[word] >> offset) | (sum.limbs[word + 1] << (WORD_BITS - offset));
    let above_u64 = carry != 0 || sum.limbs[word + 1] >> offset != 0 || sum.limbs[word + 2..].iter().any(|&w| w != 0);
    if above_u64 || integer >= remaining {
        return Ok((remaining, ExactBinarySum::default().to_amount()));
    }
    sum.limbs[word] &= (1_u64 << offset) - 1;
    sum.limbs[word + 1..].fill(0);
    Ok((integer, sum.to_amount()))
}

#[cfg(test)] mod tests;
