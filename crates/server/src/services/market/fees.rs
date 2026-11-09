//! Market fees and trade skills for marketProxy (eve/client/script/ui/services/marketsvc.py).
//! Uses character skills and standings; faction-war upgrades are unsupported.
//! Configured fee rates are used to 12 decimal places; source inputs are
//! rounded half away from zero. Standing quantization contributes at most
//! 2.5e-16 rate error, below 0.01 ISK at a 1e13 ISK principal. Client
//! literal coefficients are exact; no composite rate is quantized.
use super::{db_err, notify_error, round2, CallError, Executor};
use crate::dao::market::trading as trading_dao;
use infinity_core::consts::{SKILL_ACCOUNTING, SKILL_BROKER_RELATIONS};
use infinity_core::messages;
use crate::sde::Sde;
use crate::isk::IskAmount;

/// Source-input precision for configured rates and standings.
const FEE_INPUT_QUANTUM: i128 = 1_000_000_000_000;
/// `marketutil.skilllimits.GetBrokersFeeForLocation` and `marketutil.const.BROKER_RELATIONS_SKILL_MODIFIER`: broker relations per level.
const BROKER_RELATIONS_PER_LEVEL: FeeRate = FeeRate { num: 3, den: 1000 };
/// `marketutil.skilllimits.GetSkillLimits` and `marketutil.const.ACCOUNTING_SKILL_MODIFIER`: accounting reduction per level.
const ACCOUNTING_PER_LEVEL: FeeRate = FeeRate { num: 11, den: 100 };
/// `marketutil.brokerFee.GetBrokerFeeInfo`: faction standing contribution.
const MARKET_FACTION_STANDING_MULTIPLIER: FeeRate = FeeRate { num: 3, den: 10000 };
/// `marketutil.brokerFee.GetBrokerFeeInfo`: corporation standing contribution.
const MARKET_NPC_CORP_STANDING_MULTIPLIER: FeeRate = FeeRate { num: 2, den: 10000 };
/// `marketutil.skilllimits.GetModificationFeeDiscount`: base modification discount.
const MODIFICATION_BASE: FeeRate = FeeRate { num: 1, den: 2 };
/// `marketutil.skilllimits.GetModificationFeeDiscount`: modification discount per level.
const MODIFICATION_PER_LEVEL: FeeRate = FeeRate { num: 3, den: 50 };

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 { (a, b) = (b, a % b); }
    a
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FeeRate { num: i128, den: i128 }

impl FeeRate {
    fn ratio(num: i128, den: i128) -> Option<Self> {
        if den <= 0 { return None; }
        let gcd = i128::try_from(gcd(num.unsigned_abs(), den as u128)).ok()?;
        Some(Self { num: num / gcd, den: den / gcd })
    }

    /// Convert an original configuration or standing input, never a computed rate.
    pub(super) fn from_input(value: f64) -> Option<Self> {
        let scaled = (value * FEE_INPUT_QUANTUM as f64).round();
        if !scaled.is_finite() || scaled <= i128::MIN as f64 || scaled >= i128::MAX as f64 { return None; }
        Self::ratio(scaled as i128, FEE_INPUT_QUANTUM)
    }

    fn add(self, other: Self) -> Option<Self> {
        let divisor = i128::try_from(gcd(self.den as u128, other.den as u128)).ok()?;
        let left = other.den / divisor;
        let right = self.den / divisor;
        Self::ratio(self.num.checked_mul(left)?.checked_add(other.num.checked_mul(right)?)?, self.den.checked_mul(left)?)
    }
    fn sub(self, other: Self) -> Option<Self> {
        self.add(Self { num: other.num.checked_neg()?, den: other.den })
    }
    fn mul(self, other: Self) -> Option<Self> {
        let x = i128::try_from(gcd(self.num.unsigned_abs(), other.den as u128)).ok()?;
        let y = i128::try_from(gcd(other.num.unsigned_abs(), self.den as u128)).ok()?;
        Self::ratio((self.num / x).checked_mul(other.num / y)?, (self.den / y).checked_mul(other.den / x)?)
    }
    pub(super) fn as_f64(self) -> f64 { self.num as f64 / self.den as f64 }
}

const SKILL_TRADE: i32 = 3443;
const SKILL_RETAIL: i32 = 3444;
const SKILL_WHOLESALE: i32 = 16596;
const SKILL_MARGIN_TRADING: i32 = 16597;
const SKILL_TYCOON: i32 = 18580;
/// SDE skill type IDs, inventorycommon.const market skills.
const SKILL_MARKETING: i32 = 16598;
const SKILL_PROCUREMENT: i32 = 16594;
const SKILL_VISIBILITY: i32 = 3447;
const SKILL_DAYTRADING: i32 = 16595;
/// marketutil.skilllimits.JUMPS_PER_SKILL_LEVEL.
const SKILL_RANGES: [i64; 6] = [-1, 0, 5, 10, 20, 32767];
/// `marketutil/ticks.py`: minimum division, significant figures, and rounding guard.
const MIN_PRICE: f64 = 0.01;
const TICK_SIGNIFICANT_FIGURES: f64 = 4.0;
const TICK_ROUNDING_GUARD: f64 = MIN_PRICE / 100.0;
/// `carbon/common/lib/const.py` FLOAT_TOLERANCE, used by FloatCloseEnough.
const FLOAT_TOLERANCE: f64 = 0.001;
/// Percentage conversion used by the market quote error.
const PERCENT: f64 = 100.0;

/// `marketutil.skilllimits.JUMPS_PER_SKILL_LEVEL`; effective levels are bounded by skill rules.
fn skill_limit(level: i32) -> i64 {
    SKILL_RANGES[level.clamp(0, 5) as usize]
}

/// A finite whole-cent ISK price from the `marketutil/ticks.py` minimum to the market order price ceiling.
pub(super) fn valid_cents(price: f64) -> bool {
    price.is_finite() && (MIN_PRICE..=super::MAX_ORDER_PRICE).contains(&price) && round2(price) == price
}

/// `marketutil/ticks.py:12-21,44-46`, with the market order price ceiling.
pub(super) fn valid_tick(price: f64) -> bool {
    if !valid_cents(price) { return false; }
    let adjusted = price + TICK_ROUNDING_GUARD;
    let shift = 10_f64.powf(adjusted.log10().floor() - TICK_SIGNIFICANT_FIGURES + 1.0);
    round2((adjusted / shift).floor() * shift) == price
}

/// `buyThisTypeWindow.py` duration and range choices for ordinary market orders.
pub(super) fn valid_order_range(range: i64) -> bool {
    matches!(range, -1 | 0 | 1..=5 | 10 | 20 | 30 | 40 | 32767)
}

/// The configured maximum and expiry overflow are checked by `super::valid_duration`.
pub(super) fn valid_order_duration(duration: i64, now: i64) -> bool {
    matches!(duration, 0 | 1 | 3 | 7 | 14 | 30 | 90) && super::valid_duration(duration, now)
}

/// What a character's trade skills make of fees, taxes and order slots (marketutil.skilllimits).
pub(super) struct Skills {
    pub(super) order_cap: i64,
    pub(super) sales_tax: FeeRate,
    pub(super) broker_fee: FeeRate,
    pub(super) modification_discount: FeeRate,
    pub(super) marketing: i64,
    pub(super) procurement: i64,
    pub(super) visibility: i64,
    pub(super) daytrading: i64,
}

pub(super) async fn skills(ex: &(impl Executor + ?Sized), sde: &Sde, char_id: i64, at: i64) -> Result<Skills, CallError> {
    let levels = crate::services::skills::effective::snapshot_at(ex, sde, char_id, at).await?;
    skills_from_levels(|skill| levels.level(skill))
}

fn skills_from_levels(level: impl Fn(i32) -> i32) -> Result<Skills, CallError> {
    let l = |skill: i32| level(skill) as i64;
    let factor = |skill| FeeRate { num: i128::from(level(skill)), den: 1 };
    let one = FeeRate { num: 1, den: 1 };
    let market = &crate::config::game().market;
    let tax = FeeRate::from_input(market.sales_tax).ok_or_else(fee_error)?;
    let broker = FeeRate::from_input(market.broker_fee).ok_or_else(fee_error)?;
    let sales_tax = ACCOUNTING_PER_LEVEL.mul(factor(SKILL_ACCOUNTING)).and_then(|v| one.sub(v)).and_then(|v| tax.mul(v)).ok_or_else(fee_error)?;
    let broker_fee = BROKER_RELATIONS_PER_LEVEL.mul(factor(SKILL_BROKER_RELATIONS)).and_then(|v| broker.sub(v)).ok_or_else(fee_error)?;
    let modification_discount = MODIFICATION_PER_LEVEL.mul(factor(SKILL_MARGIN_TRADING)).and_then(|v| MODIFICATION_BASE.add(v)).ok_or_else(fee_error)?;
    Ok(Skills {
        order_cap: 5 + 4 * l(SKILL_TRADE) + 8 * l(SKILL_RETAIL) + 16 * l(SKILL_WHOLESALE) + 32 * l(SKILL_TYCOON),
        sales_tax, broker_fee, modification_discount,
        marketing: skill_limit(level(SKILL_MARKETING)),
        procurement: skill_limit(level(SKILL_PROCUREMENT)),
        visibility: skill_limit(level(SKILL_VISIBILITY)),
        daytrading: skill_limit(level(SKILL_DAYTRADING)),
    })
}
/// `marketutil/brokerFee.py:63-70`; faction-war upgrade level zero is identity.
pub(super) fn adjusted_broker_rate(commission: FeeRate, faction: f64, corp: f64) -> Result<FeeRate, CallError> {
    let faction = FeeRate::from_input(faction).and_then(|v| v.mul(MARKET_FACTION_STANDING_MULTIPLIER)).ok_or_else(fee_error)?;
    let corp = FeeRate::from_input(corp).and_then(|v| v.mul(MARKET_NPC_CORP_STANDING_MULTIPLIER)).ok_or_else(fee_error)?;
    commission.sub(faction).and_then(|v| v.sub(corp)).ok_or_else(fee_error)
}

pub(super) async fn station_broker_rate(ex: &(impl Executor + ?Sized), sde: &crate::sde::Sde, char_id: i64, owner: i64, commission: FeeRate) -> Result<FeeRate, CallError> {
    let Some(corp) = sde.npc_corporations.get(owner) else { return Ok(commission) };
    let corp_standing = crate::services::standings::standing(ex, char_id, owner).await?;
    let faction_standing = match corp.faction_id {
        Some(faction) => crate::services::standings::standing(ex, char_id, faction).await?,
        None => 0.0,
    };
    adjusted_broker_rate(commission, faction_standing, corp_standing)
}

fn fee_error() -> CallError { notify_error(messages::INVALID_PRICE) }

/// Quantize once, to nearest cent with ties away from zero.
pub(super) fn apply_rate(amount: IskAmount, rate: FeeRate) -> Result<IskAmount, CallError> {
    if amount.is_negative() || rate.num < 0 { return Err(fee_error()); }
    amount.checked_mul_ratio(rate.num as u128, rate.den as u128).ok_or_else(fee_error)
}

fn minimum_fee() -> Result<IskAmount, CallError> {
    let minimum = IskAmount::from_isk(crate::config::game().market.broker_fee_min_isk).map_err(|_| fee_error())?;
    if minimum.is_negative() { return Err(fee_error()); }
    Ok(minimum)
}

pub(super) fn broker_fee(rate: FeeRate, amount: IskAmount) -> Result<IskAmount, CallError> {
    let rate = if rate.num < 0 { FeeRate { num: 0, den: 1 } } else { rate };
    Ok(apply_rate(amount, rate)?.max(minimum_fee()?))
}

/// `marketutil.brokerFee.GetBrokerFeeInfo`: round the combined modification
/// and positive-increase components only once, then apply the broker minimum.
pub(super) fn modification_fee(rate: FeeRate, old_notional: IskAmount, new_notional: IskAmount, discount: FeeRate) -> Result<IskAmount, CallError> {
    if old_notional.is_negative() || new_notional.is_negative() || discount.num < 0 || discount.num > discount.den { return Err(fee_error()); }
    let rate = if rate.num < 0 { FeeRate { num: 0, den: 1 } } else { rate };
    let delta = if new_notional > old_notional { new_notional.checked_sub(old_notional).ok_or_else(fee_error)? } else { IskAmount::from_cents(0).ok_or_else(fee_error)? };
    let denominator = (rate.den as u128).checked_mul(discount.den as u128).ok_or_else(fee_error)?;
    let first = (rate.num as u128).checked_mul((discount.den - discount.num) as u128).ok_or_else(fee_error)?;
    let second = (rate.num as u128).checked_mul(discount.den as u128).ok_or_else(fee_error)?;
    let fee = new_notional.checked_weighted_sum(first, delta, second, denominator).ok_or_else(fee_error)?;
    Ok(fee.max(minimum_fee()?))
}
pub(super) fn check_expected_fee(expected: Option<f64>, rate: FeeRate) -> Result<(), CallError> {
    let rate = rate.as_f64();
    if expected.is_some_and(|value| !value.is_finite() || (value - rate).abs() >= FLOAT_TOLERANCE) {
        return Err(crate::services::user_error(messages::MSG_MKT_BROKERS_FEE_UNEXPECTED, vec![
            ("actualBrokerFeePerc".into(), marshal::Value::Float(PERCENT * rate)),
            ("expectedBrokerFeePercentage".into(), expected.map_or(marshal::Value::None, marshal::Value::Float)),
        ]));
    }
    Ok(())
}

pub(super) fn check_remote(identity: &crate::services::identity::Identity, sde: &Sde, station: &super::Station, range: i64) -> Result<(), CallError> {
    let allowed = if identity.station_id == Some(station.id) {
        range >= super::RANGE_STATION
    } else if range < super::RANGE_SOLAR_SYSTEM {
        false
    } else if let Some(system) = identity.system_id {
        sde.map.system(system).is_some_and(|s| s.region_id == Some(station.region))
            && super::jumps(&sde.map, system, station.system, range).is_some()
    } else {
        false
    };
    if allowed { Ok(()) } else { Err(notify_error(messages::market::REMOTE_TRADE_OUT_OF_RANGE)) }
}

pub(super) async fn check_order_slots(db: &(impl Executor + ?Sized), char_id: i64, skills: &Skills) -> Result<(), CallError> {
    let open = trading_dao::order_count(db, char_id).await.map_err(db_err)?;
    if open >= skills.order_cap {
        return Err(notify_error(&messages::market_order_limit_message(open)));
    }
    Ok(())
}


#[cfg(test)]
mod tests;
