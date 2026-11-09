//! Typed game policy defaults and validation. These values are server policy, not SDE data.
//! They do not change client-computed formulas or migrate gameplay consumers.

use anyhow::Result;
use serde::Deserialize;

/// Largest accepted starting PLEX. Native TOML integer domain is `0..=i64::MAX`.
/// Server policy: starting PLEX is a native i64 in that range.
const MAX_STARTING_PLEX: i64 = i64::MAX;

/// 15 significant digits (f64 DBL_DIG): the largest two-decimal amount that survives decimal → f64 → PostgreSQL float8::numeric unchanged.
const MAX_STARTING_ISK: f64 = 9_999_999_999_999.99;
/// Station repair pricing policy.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepairConfig {
    /// Server policy: multiplier applied to computed base price per hitpoint.
    pub cost_multiplier: f64,
}
impl Default for RepairConfig {
    fn default() -> Self { Self { cost_multiplier: 1.0 } }
}
impl RepairConfig {
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.cost_multiplier.is_finite() && self.cost_multiplier > 0.0, "repair.cost_multiplier must be finite and positive");
        Ok(())
    }
}


/// Direct trade policy.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TradeConfig {
    /// Server policy: idle trade timeout, default 1800 seconds.
    pub idle_timeout_secs: i64,
}

impl Default for TradeConfig {
    fn default() -> Self { Self { idle_timeout_secs: 1800 } }
}

impl TradeConfig {
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.idle_timeout_secs > 0, "trade.idle_timeout_secs must be positive");
        super::duration("trade.idle_timeout_secs", self.idle_timeout_secs, infinity_core::consts::FILETIME_SECOND, true)?;
        Ok(())
    }
}
/// Daily free offer from services/store/store_manager.rs.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DailyGift {
    /// Gift item type, from the existing free daily offer.
    pub type_id: i64,
    /// Gift quantity, from the existing free daily offer.
    pub quantity: i64,
}
impl Default for DailyGift {
    fn default() -> Self { Self { type_id: 52520, quantity: 1 } }
}
impl DailyGift {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!((1..=i64::from(i32::MAX)).contains(&self.type_id), "store.daily_gift.type_id={} must fit a positive i32", self.type_id);
        anyhow::ensure!(self.quantity > 0, "store.daily_gift.quantity={} must be positive", self.quantity);
        Ok(())
    }
}

/// Accounts configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountsConfig {
    /// Existing SQL default for the account PLEX vault.
    pub starting_plex: i64,
    /// Server character-selection slots per account.
    pub character_slots: i64,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            starting_plex: 1000,
            character_slots: 3,
        }
    }
}

impl AccountsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!((0..=MAX_STARTING_PLEX).contains(&self.starting_plex), "accounts.starting_plex={} is outside its valid domain", self.starting_plex);
        anyhow::ensure!(self.character_slots > 0, "accounts.character_slots={} is outside its valid domain", self.character_slots);
        Ok(())
    }
}

/// Characters configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CharactersConfig {
    /// Server starting balance policy in services/charsel.rs.
    pub starting_isk: f64,
    /// Minimum length in server character-name validation.
    pub name_min_len: i64,
    /// Maximum length in server character-name validation.
    pub name_max_len: i64,
    /// Maximum spaces in server character-name validation.
    pub name_max_spaces: i64,
    /// Server deletion delay guards against a stray click.
    pub delete_delay_secs: i64,
    /// Server design default for free respecs in services/skills/skill_handler.rs.
    pub free_respecs: i64,
}

impl Default for CharactersConfig {
    fn default() -> Self {
        Self {
            starting_isk: 5000000.0,
            name_min_len: 3,
            name_max_len: 37,
            name_max_spaces: 2,
            delete_delay_secs: 30,
            free_respecs: 1,
        }
    }
}

impl CharactersConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.starting_isk.is_finite() && self.starting_isk >= 0.0 && self.starting_isk <= MAX_STARTING_ISK, "characters.starting_isk={} is outside its valid domain", self.starting_isk);
        anyhow::ensure!(self.name_min_len > 0, "characters.name_min_len={} is outside its valid domain", self.name_min_len);
        anyhow::ensure!(self.name_max_len > 0, "characters.name_max_len={} is outside its valid domain", self.name_max_len);
        anyhow::ensure!(self.name_max_spaces > 0, "characters.name_max_spaces={} is outside its valid domain", self.name_max_spaces);
        anyhow::ensure!(self.delete_delay_secs > 0, "characters.delete_delay_secs={} is outside its valid domain", self.delete_delay_secs);
        super::duration("characters.delete_delay_secs", self.delete_delay_secs, infinity_core::consts::FILETIME_SECOND, true)?;
        anyhow::ensure!(self.free_respecs > 0, "characters.free_respecs={} is outside its valid domain", self.free_respecs);
        anyhow::ensure!(self.name_min_len <= self.name_max_len, "characters.name_min_len={} exceeds characters.name_max_len={}", self.name_min_len, self.name_max_len);
        Ok(())
    }
}

/// Market configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarketConfig {
    /// Default broker fee from marketutil.const.
    pub broker_fee: f64,
    /// Minimum broker fee from appConst.mktMinimumFee.
    pub broker_fee_min_isk: f64,
    /// Transaction tax from MKT_TRANSACTION_TAX_DEFAULT.
    pub sales_tax: f64,
    /// Server design default for order duration in services/market/mod.rs.
    pub max_order_days: i64,
    /// Server policy for order modification cooldown, default from appConst.mktModificationDelay; 0 disables it.
    pub modification_delay_seconds: i64,
    /// Server market policy: NPC buy price as a fraction of sell price.
    pub npc_buy_ratio: f64,
    /// Server market order maintenance cadence.
    pub maintenance_secs: i64,
    /// Server closed-order history retention.
    pub order_history_days: i64,
    /// Server default transaction-history period.
    pub transaction_history_days: i64,
    /// Server market price-history retention.
    pub price_history_days: i64,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            broker_fee: 0.03,
            broker_fee_min_isk: 100.0,
            sales_tax: 0.08,
            max_order_days: 90,
            modification_delay_seconds: 300,
            npc_buy_ratio: 0.8,
            maintenance_secs: 60,
            order_history_days: 90,
            transaction_history_days: 30,
            price_history_days: 365,
        }
    }
}

impl MarketConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.broker_fee.is_finite() && (0.0..=1.0).contains(&self.broker_fee), "market.broker_fee={} is outside its valid domain", self.broker_fee);
        anyhow::ensure!(self.broker_fee_min_isk.is_finite() && self.broker_fee_min_isk >= 0.0, "market.broker_fee_min_isk={} is outside its valid domain", self.broker_fee_min_isk);
        anyhow::ensure!(self.sales_tax.is_finite() && (0.0..=1.0).contains(&self.sales_tax), "market.sales_tax={} is outside its valid domain", self.sales_tax);
        anyhow::ensure!((1..=i64::from(i16::MAX)).contains(&self.max_order_days), "market.max_order_days={} must fit a positive i16", self.max_order_days);
        anyhow::ensure!(self.modification_delay_seconds >= 0, "market.modification_delay_seconds={} must be nonnegative", self.modification_delay_seconds);
        super::duration("market.modification_delay_seconds", self.modification_delay_seconds, infinity_core::consts::FILETIME_SECOND, false)?;
        anyhow::ensure!(self.npc_buy_ratio.is_finite() && (0.0..=1.0).contains(&self.npc_buy_ratio), "market.npc_buy_ratio={} is outside its valid domain", self.npc_buy_ratio);
        anyhow::ensure!(self.maintenance_secs > 0, "market.maintenance_secs={} is outside its valid domain", self.maintenance_secs);
        anyhow::ensure!(self.order_history_days > 0, "market.order_history_days={} is outside its valid domain", self.order_history_days);
        anyhow::ensure!(self.transaction_history_days > 0, "market.transaction_history_days={} is outside its valid domain", self.transaction_history_days);
        anyhow::ensure!(self.price_history_days > 0, "market.price_history_days={} is outside its valid domain", self.price_history_days);
        for (key, value) in [("market.order_history_days", self.order_history_days), ("market.transaction_history_days", self.transaction_history_days), ("market.price_history_days", self.price_history_days)] {
            super::duration(key, value, infinity_core::consts::FILETIME_DAY, false)?;
        }
        Ok(())
    }
}

/// Store configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Server design default fallback price in services/store/mod.rs.
    pub plex_price_isk_fallback: f64,
    /// The daily free gift: 10,000 skill points, granted by one type 52520 token.
    pub daily_gift: DailyGift,
    /// Daily turnover of login rewards and daily offers (UTC; 11:00 Beijing time).
    /// Existing server policy: daily reward turnover offset from midnight UTC, in seconds.
    pub daily_turnover_utc: String,
    /// Server design default for checkout in services/store/store_manager.rs.
    pub fast_checkout: bool,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            plex_price_isk_fallback: 23500000.0,
            daily_gift: DailyGift::default(),
            daily_turnover_utc: "03:00:00".to_owned(),
            fast_checkout: false,
        }
    }
}

impl StoreConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.plex_price_isk_fallback.is_finite() && self.plex_price_isk_fallback > 0.0, "store.plex_price_isk_fallback={} is outside its valid domain", self.plex_price_isk_fallback);
        self.daily_gift.validate()?;
        let mut parts = self.daily_turnover_utc.split(':');
        let valid_part = |part: Option<&str>, limit| {
            part.is_some_and(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u8>().is_ok_and(|v| v < limit))
        };
        let valid = valid_part(parts.next(), 24) && valid_part(parts.next(), 60)
            && valid_part(parts.next(), 60) && parts.next().is_none();
        anyhow::ensure!(valid, "store.daily_turnover_utc={:?} must be a UTC time HH:MM:SS", self.daily_turnover_utc);

        Ok(())
    }
}

/// Industry configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndustryConfig {
    /// Industry job cost rate from industry.const in services/industry/mod.rs.
    pub job_cost_rate: f64,
    /// SCC surcharge from industry.const in services/industry/mod.rs.
    pub scc_surcharge_rate: f64,
    /// security.common.const.INDUSTRY_SECURITY_TAX_RATE, high-sec only.
    pub highsec_security_tax: f64,
    /// What an NPC station's industry service charges on the estimated item value.
    pub npc_facility_tax: f64,
    /// The system cost index of every activity everywhere (there is no activity tracking).
    pub system_cost_index: f64,
    /// Invention skill probability from industry.const in services/industry/mod.rs.
    pub invention_skill_probability: f64,
    /// Server design default duration multiplier in services/industry/mod.rs.
    pub job_time_multiplier: f64,
    /// Server design default maximum run duration in services/industry/mod.rs.
    pub max_run_secs: f64,
    /// Server design default run limit in services/industry/mod.rs.
    pub max_runs: i64,
    /// Existing server policy for completed industry job history, in days.
    pub history_days: i64,
}

impl Default for IndustryConfig {
    fn default() -> Self {
        Self {
            job_cost_rate: 0.02,
            scc_surcharge_rate: 0.04,
            highsec_security_tax: 0.05,
            npc_facility_tax: 0.0025,
            system_cost_index: 0.02,
            invention_skill_probability: 1.0 / 30.0,
            job_time_multiplier: 1.0,
            max_run_secs: 2592000.0,
            max_runs: 1000000,
            history_days: 90,
        }
    }
}

impl IndustryConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.job_cost_rate.is_finite() && (0.0..=1.0).contains(&self.job_cost_rate), "industry.job_cost_rate={} is outside its valid domain", self.job_cost_rate);
        anyhow::ensure!(self.scc_surcharge_rate.is_finite() && (0.0..=1.0).contains(&self.scc_surcharge_rate), "industry.scc_surcharge_rate={} is outside its valid domain", self.scc_surcharge_rate);
        anyhow::ensure!(self.highsec_security_tax.is_finite() && (0.0..=1.0).contains(&self.highsec_security_tax), "industry.highsec_security_tax={} is outside its valid domain", self.highsec_security_tax);
        anyhow::ensure!(self.npc_facility_tax.is_finite() && (0.0..=1.0).contains(&self.npc_facility_tax), "industry.npc_facility_tax={} is outside its valid domain", self.npc_facility_tax);
        anyhow::ensure!(self.system_cost_index.is_finite() && (0.0..=1.0).contains(&self.system_cost_index), "industry.system_cost_index={} is outside its valid domain", self.system_cost_index);
        anyhow::ensure!(self.invention_skill_probability.is_finite() && (0.0..=1.0).contains(&self.invention_skill_probability), "industry.invention_skill_probability={} is outside its valid domain", self.invention_skill_probability);
        anyhow::ensure!(self.job_time_multiplier.is_finite() && self.job_time_multiplier > 0.0, "industry.job_time_multiplier={} is outside its valid domain", self.job_time_multiplier);
        anyhow::ensure!(self.max_run_secs.is_finite() && self.max_run_secs > 0.0, "industry.max_run_secs={} is outside its valid domain", self.max_run_secs);
        anyhow::ensure!(self.max_runs > 0, "industry.max_runs={} is outside its valid domain", self.max_runs);
        anyhow::ensure!(self.history_days > 0, "industry.history_days={} is outside its valid domain", self.history_days);
        Ok(())
    }
}

