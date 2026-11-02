//! Typed game policy defaults and validation. These values are server policy, not SDE data.
//! They do not change client-computed formulas or migrate gameplay consumers.

use anyhow::Result;
use serde::Deserialize;

/// Client ceiling from evefleet/const.py RECONNECT_TIMEOUT: two minutes.
const FLEET_RECONNECT_CEILING_SECS: u64 = 120;

/// Server policy for retaining disconnected fleet seats.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FleetsConfig {
    /// Seat retention in seconds, capped by the client's reconnect window.
    pub reconnect_timeout_secs: u64,
}

impl Default for FleetsConfig {
    fn default() -> Self {
        Self { reconnect_timeout_secs: FLEET_RECONNECT_CEILING_SECS }
    }
}

impl FleetsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=FLEET_RECONNECT_CEILING_SECS).contains(&self.reconnect_timeout_secs),
            "fleets.reconnect_timeout_secs={} must be between 1 and {}: the client stops reconnecting after two minutes",
            self.reconnect_timeout_secs, FLEET_RECONNECT_CEILING_SECS
        );
        Ok(())
    }
}

/// Corporations configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorporationsConfig {
    /// Minimum Corporation Management skill to found or lead a corporation.
    pub min_management_skill_level: i64,
    /// Shares issued at founding and in the one-time NULL-total startup backfill.
    /// Later policy changes do not alter existing issued totals.
    pub initial_shares: i64,
    /// Corporation and alliance bulletin limit from base_corporation.MAX_NUM_BULLETINS.
    pub max_bulletins: i64,
    /// Server office room capacity; no capacity was shipped in client or SDE.
    pub office_slots_per_station: i64,
    /// Server minimum office rent.
    pub office_rent_base_isk: f64,
    /// Additional office rent in ISK at security 1.
    pub office_rent_per_security_isk: f64,
    /// Server billing maintenance cadence in services/bills.rs.
    pub bills_maintenance_secs: i64,
    /// Server advance rent billing policy in services/bills.rs.
    pub advance_billing_days: i64,
}

impl Default for CorporationsConfig {
    fn default() -> Self {
        Self {
            min_management_skill_level: 1,
            initial_shares: 1000,
            max_bulletins: 10,
            office_slots_per_station: 24,
            office_rent_base_isk: 100000.0,
            office_rent_per_security_isk: 900000.0,
            bills_maintenance_secs: 600,
            advance_billing_days: 7,
        }
    }
}

impl CorporationsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!((1..=5).contains(&self.min_management_skill_level), "corporations.min_management_skill_level={} is outside its valid domain", self.min_management_skill_level);
        anyhow::ensure!(self.initial_shares > 0, "corporations.initial_shares={} is outside its valid domain", self.initial_shares);
        anyhow::ensure!(self.max_bulletins > 0, "corporations.max_bulletins={} is outside its valid domain", self.max_bulletins);
        anyhow::ensure!(self.office_slots_per_station > 0, "corporations.office_slots_per_station={} is outside its valid domain", self.office_slots_per_station);
        anyhow::ensure!(self.office_rent_base_isk.is_finite() && self.office_rent_base_isk >= 0.0, "corporations.office_rent_base_isk={} is outside its valid domain", self.office_rent_base_isk);
        anyhow::ensure!(self.office_rent_per_security_isk.is_finite() && self.office_rent_per_security_isk >= 0.0, "corporations.office_rent_per_security_isk={} is outside its valid domain", self.office_rent_per_security_isk);
        anyhow::ensure!((self.office_rent_base_isk + self.office_rent_per_security_isk).is_finite(), "corporations.office_rent_base_isk={} plus corporations.office_rent_per_security_isk={} must be finite", self.office_rent_base_isk, self.office_rent_per_security_isk);
        anyhow::ensure!(self.bills_maintenance_secs > 0, "corporations.bills_maintenance_secs={} is outside its valid domain", self.bills_maintenance_secs);
        anyhow::ensure!(self.advance_billing_days > 0, "corporations.advance_billing_days={} is outside its valid domain", self.advance_billing_days);
        super::duration("corporations.advance_billing_days", self.advance_billing_days, crate::consts::FILETIME_DAY, true)?;
        Ok(())
    }
}

/// Contracts configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContractsConfig {
    /// Existing server auction maintenance cadence.
    pub maintenance_secs: i64,
    /// Server design default minimum expiry in services/contracts/lifecycle.rs.
    pub expiry_min_minutes: i64,
    /// Server design default maximum expiry in services/contracts/lifecycle.rs.
    pub expiry_max_days: i64,
    /// Server design default minimum courier duration in services/contracts/lifecycle.rs.
    pub courier_min_days: i64,
    /// Server design default maximum courier duration in services/contracts/lifecycle.rs.
    pub courier_max_days: i64,
    /// Server design default item limit in services/contracts/lifecycle.rs.
    pub max_items: i64,
    /// Server design default title limit in services/contracts/lifecycle.rs.
    pub title_max_len: i64,
    /// Server design default description limit in services/contracts/lifecycle.rs.
    pub description_max_len: i64,
    /// Server design default search page size in services/contracts.
    pub page_size: i64,
    /// Server design default maximum search page size in services/contracts.
    pub page_size_max: i64,
}

impl Default for ContractsConfig {
    fn default() -> Self {
        Self {
            maintenance_secs: 60,
            expiry_min_minutes: 60,
            expiry_max_days: 90,
            courier_min_days: 1,
            courier_max_days: 365,
            max_items: 500,
            title_max_len: 100,
            description_max_len: 4000,
            page_size: 100,
            page_size_max: 1000,
        }
    }
}

impl ContractsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.maintenance_secs > 0, "contracts.maintenance_secs={} is outside its valid domain", self.maintenance_secs);
        anyhow::ensure!(self.expiry_min_minutes > 0, "contracts.expiry_min_minutes={} is outside its valid domain", self.expiry_min_minutes);
        anyhow::ensure!(self.expiry_max_days > 0, "contracts.expiry_max_days={} is outside its valid domain", self.expiry_max_days);
        anyhow::ensure!(self.courier_min_days > 0, "contracts.courier_min_days={} is outside its valid domain", self.courier_min_days);
        anyhow::ensure!(self.courier_max_days > 0, "contracts.courier_max_days={} is outside its valid domain", self.courier_max_days);
        anyhow::ensure!(self.courier_max_days <= i64::from(i32::MAX), "contracts.courier_max_days={} must fit i32", self.courier_max_days);
        super::duration("contracts.expiry_min_minutes", self.expiry_min_minutes, crate::consts::FILETIME_MINUTE, true)?;
        super::duration("contracts.expiry_max_days", self.expiry_max_days, crate::consts::FILETIME_DAY, true)?;
        super::duration("contracts.courier_max_days", self.courier_max_days, crate::consts::FILETIME_DAY, true)?;
        anyhow::ensure!(self.max_items > 0, "contracts.max_items={} is outside its valid domain", self.max_items);
        anyhow::ensure!(self.title_max_len > 0, "contracts.title_max_len={} is outside its valid domain", self.title_max_len);
        anyhow::ensure!(self.description_max_len > 0, "contracts.description_max_len={} is outside its valid domain", self.description_max_len);
        anyhow::ensure!(self.page_size > 0, "contracts.page_size={} is outside its valid domain", self.page_size);
        anyhow::ensure!(self.page_size_max > 0, "contracts.page_size_max={} is outside its valid domain", self.page_size_max);
        anyhow::ensure!(self.courier_min_days <= self.courier_max_days, "contracts.courier_min_days={} exceeds contracts.courier_max_days={}", self.courier_min_days, self.courier_max_days);
        anyhow::ensure!(self.page_size <= self.page_size_max, "contracts.page_size={} exceeds contracts.page_size_max={}", self.page_size, self.page_size_max);
        anyhow::ensure!(i128::from(self.expiry_min_minutes) <= i128::from(self.expiry_max_days) * 1440, "contracts.expiry_min_minutes={} exceeds contracts.expiry_max_days={}", self.expiry_min_minutes, self.expiry_max_days);
        Ok(())
    }
}

/// Clones configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClonesConfig {
    /// Server base jump-clone cooldown in services/clones.rs.
    pub jump_cooldown_hours: i64,
    /// One hour less cooldown per Infomorph Synchronizing level in services/clones.rs.
    pub synchronizing_hours_per_level: i64,
}

impl Default for ClonesConfig {
    fn default() -> Self {
        Self {
            jump_cooldown_hours: 24,
            synchronizing_hours_per_level: 1,
        }
    }
}

impl ClonesConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.jump_cooldown_hours > 0, "clones.jump_cooldown_hours={} is outside its valid domain", self.jump_cooldown_hours);
        anyhow::ensure!(self.synchronizing_hours_per_level > 0, "clones.synchronizing_hours_per_level={} is outside its valid domain", self.synchronizing_hours_per_level);
        super::duration("clones.jump_cooldown_hours", self.jump_cooldown_hours, crate::consts::FILETIME_HOUR, true)?;
        Ok(())
    }
}

/// Mail configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MailConfig {
    /// Default source: services/mail/mail_mgr.rs server mail policy.
    pub title_max_len: i64,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            title_max_len: 150,
        }
    }
}

impl MailConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.title_max_len > 0, "mail.title_max_len={} is outside its valid domain", self.title_max_len);
        Ok(())
    }
}

/// Calendar configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    /// Server design default invitee limit in services/calendar/mod.rs.
    pub max_invitees: i64,
    /// Server design default title limit in services/calendar/mod.rs.
    pub title_max_len: i64,
    /// Server design default description limit in services/calendar/mod.rs.
    pub description_max_len: i64,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            max_invitees: 50,
            title_max_len: 100,
            description_max_len: 10000,
        }
    }
}

impl CalendarConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.max_invitees > 0, "calendar.max_invitees={} is outside its valid domain", self.max_invitees);
        anyhow::ensure!(self.title_max_len > 0, "calendar.title_max_len={} is outside its valid domain", self.title_max_len);
        anyhow::ensure!(self.description_max_len > 0, "calendar.description_max_len={} is outside its valid domain", self.description_max_len);
        Ok(())
    }
}

/// Space configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpaceConfig {
    /// Issue 4: persist changed positions every 30 seconds, bounding default crash loss.
    pub position_snapshot_secs: i64,
    /// Server policy: mailbox depth above which a space actor logs a warning.
    pub mailbox_budget: i64,
}

impl Default for SpaceConfig {
    fn default() -> Self {
        Self {
            position_snapshot_secs: 30,
            mailbox_budget: 10000,
        }
    }
}

impl SpaceConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.position_snapshot_secs > 0, "space.position_snapshot_secs={} is outside its valid domain", self.position_snapshot_secs);
        super::milliseconds("space.position_snapshot_secs", self.position_snapshot_secs)?;
        anyhow::ensure!(self.mailbox_budget > 0, "space.mailbox_budget={} is outside its valid domain", self.mailbox_budget);
        Ok(())
    }
}

