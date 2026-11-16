//! Repairable-ship recovery lifecycle timing policy, sourced from server defaults
//! and operator TOML. The persisted deadlines are FILETIME values; the client's
//! own repair durations come from SDE data, not this policy.

use anyhow::Result;
use serde::Deserialize;

/// Payment, delivery and withdrawal deadlines for one recovery group.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepairableShipConfig {
    /// Server policy: days a recovery group's members await payment; default 30.
    pub payment_retention_days: i64,
    /// Server policy: delivery leg added after the later of payment and repair completion, in seconds; default 3600.
    pub delivery_seconds: i64,
    /// Server policy: days a delivered member remains withdrawable; default 30.
    pub withdrawal_retention_days: i64,
}

impl Default for RepairableShipConfig {
    fn default() -> Self {
        Self { payment_retention_days: 30, delivery_seconds: 3600, withdrawal_retention_days: 30 }
    }
}

impl RepairableShipConfig {
    pub(super) fn validate(&self) -> Result<()> {
        super::positive_duration("repairable_ship.payment_retention_days", self.payment_retention_days, infinity_core::consts::FILETIME_DAY)?;
        super::positive_duration("repairable_ship.delivery_seconds", self.delivery_seconds, infinity_core::consts::FILETIME_SECOND)?;
        super::positive_duration("repairable_ship.withdrawal_retention_days", self.withdrawal_retention_days, infinity_core::consts::FILETIME_DAY)
    }
}

#[cfg(all(test, feature = "tests-runtime"))]
#[path = "repairable_ship/tests.rs"]
mod tests;
