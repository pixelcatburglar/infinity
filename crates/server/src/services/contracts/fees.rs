//! Pure contract creation fee calculator, mirroring `eve/common/script/util/contractscommon.py`
//! (`CalcContractFees` and `CalculateDeposit`) and the private override in
//! `eve/client/script/ui/shared/neocom/contracts/createContractWnd.py` (`GetFeeTextAndAmounts`).
//! Money, duration and effective skill levels are supplied by the caller; no tables are read.
//! Faction-warfare fee upgrades are unsupported and treated as level zero.

use super::TYPE_COURIER;

/// `eve/common/script/util/contractscommon.py:13`: const_conSalesTax.
const SALES_TAX_RATE: f64 = 0.01;
/// `eve/common/script/util/contractscommon.py:89`: accounting reduction per level.
const ACCOUNTING_RATE_REDUCTION: f64 = 0.001;
/// `eve/common/script/util/contractscommon.py:14`: const_conSalesTaxMinimum.
const SALES_TAX_MINIMUM: f64 = 10_000.0;
/// `eve/common/script/util/contractscommon.py:15`: const_conSalesTaxMaximum.
const SALES_TAX_MAXIMUM: f64 = 1_000_000_000.0;
/// `eve/common/script/util/contractscommon.py:93`: accounting maximum reduction per level.
const ACCOUNTING_CAP_REDUCTION: f64 = 0.1;
/// `eve/common/script/util/contractscommon.py:16`: const_conBrokersFee.
const BROKER_RATE: f64 = 0.004;
/// `eve/common/script/util/contractscommon.py:95`: broker relations reduction per level.
const BROKER_RATE_REDUCTION: f64 = 0.0002;
/// `eve/common/script/util/contractscommon.py:17` and
/// `eve/client/script/ui/shared/neocom/contracts/createContractWnd.py:634`: minimum and private fee.
const BROKER_MINIMUM: f64 = 10_000.0;
/// `eve/common/script/util/contractscommon.py:18`: const_conBrokersFeeMaximum.
const BROKER_MAXIMUM: f64 = 100_000_000.0;
/// `eve/common/script/util/contractscommon.py:102`: maximum reduction per broker level.
const BROKER_CAP_REDUCTION_PER_LEVEL: f64 = 0.05;
/// `eve/common/script/util/contractscommon.py:104`: extra-day fee multiplier.
const BROKER_EXTRA_DAY_MULTIPLIER: f64 = 0.05;
/// `eve/common/script/util/contractscommon.py:19`: const_conDeposit.
const DEPOSIT_RATE: f64 = 0.01;
/// `eve/common/script/util/contractscommon.py:20`: const_conDepositMinimum.
const DEPOSIT_MINIMUM: f64 = 100_000.0;
/// `eve/common/script/util/contractscommon.py:21`: const_conDepositMaximum.
const DEPOSIT_MAXIMUM: f64 = 100_000_000.0;
/// `eve/common/script/util/contractscommon.py:69`: courier base cost multiplier.
const COURIER_MULTIPLIER: f64 = 0.1;
/// `eve/common/script/util/contractscommon.py:75`: courier minimum divisor.
const COURIER_DEPOSIT_DIVISOR: f64 = 10.0;
/// `eve/common/script/util/contractscommon.py:86`: minutes per day.
const MINUTES_PER_DAY: f64 = 1440.0;

pub(super) struct Fees {
    pub sales_tax: f64,
    pub sales_tax_rate: f64,
    pub sales_tax_maximum: f64,
    pub brokers_fee: f64,
    pub deposit: f64,
}

/// Apply the creation-time tax rate and maximum to a sale, including the client minimum.
/// A zero rate represents a private contract and is exempt from tax.
pub(super) fn sales_tax_amount(base: f64, rate: f64, maximum: f64) -> f64 {
    if rate == 0.0 {
        return 0.0;
    }
    (base * rate).max(SALES_TAX_MINIMUM).min(maximum).trunc()
}

/// Caller supplies a supported contract kind, finite nonnegative money and skill levels 0..=5.
pub(super) fn calculate(
    kind: i32,
    private: bool,
    price: f64,
    reward: f64,
    minutes: i128,
    accounting: i32,
    broker_relations: i32,
) -> Fees {
    if private {
        return Fees { sales_tax: 0.0, sales_tax_rate: 0.0, sales_tax_maximum: 0.0, brokers_fee: BROKER_MINIMUM, deposit: 0.0 };
    }

    let base = price.max(reward);
    let multiplier = if kind == TYPE_COURIER { COURIER_MULTIPLIER } else { 1.0 };
    let additional_days = ((minutes as f64 / MINUTES_PER_DAY).trunc()) - 1.0;
    let sales_tax_rate = (SALES_TAX_RATE - f64::from(accounting) * ACCOUNTING_RATE_REDUCTION) * multiplier;
    let sales_tax_maximum = SALES_TAX_MAXIMUM * (1.0 - f64::from(accounting) * ACCOUNTING_CAP_REDUCTION);
    let sales_tax = sales_tax_amount(base, sales_tax_rate, sales_tax_maximum);
    let broker_rate = (BROKER_RATE - f64::from(broker_relations) * BROKER_RATE_REDUCTION) * multiplier;
    let broker_cap = BROKER_MAXIMUM * (1.0 - f64::from(broker_relations) * BROKER_CAP_REDUCTION_PER_LEVEL);
    let brokers_fee = (base * broker_rate).max(BROKER_MINIMUM).min(broker_cap).trunc()
        * (1.0 + additional_days * BROKER_EXTRA_DAY_MULTIPLIER);
    let deposit_minimum = if kind == TYPE_COURIER { DEPOSIT_MINIMUM / COURIER_DEPOSIT_DIVISOR } else { DEPOSIT_MINIMUM };
    let deposit = (base * (DEPOSIT_RATE * multiplier)).max(deposit_minimum).min(DEPOSIT_MAXIMUM).trunc();

    Fees { sales_tax, sales_tax_rate, sales_tax_maximum, brokers_fee, deposit }
}

#[cfg(test)]
mod tests;
