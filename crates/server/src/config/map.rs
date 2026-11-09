//! Operator `[map]` policy for event retention in hours and maintenance in seconds,
//! not FILETIME units; it has no direct client mirror. It feeds precise FILETIME
//! event retention and the existing scheduler, not visit expiry.
use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MapConfig {
    /// Server retention horizon for map events, in hours (at least 24).
    pub history_retention_hours: i64,
    /// Server event cleanup interval, in seconds.
    pub maintenance_secs: i64,
}

impl Default for MapConfig {
    fn default() -> Self {
        Self { history_retention_hours: 48, maintenance_secs: 3600 }
    }
}

impl MapConfig {
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.history_retention_hours >= 24, "map.history_retention_hours={} must be at least 24", self.history_retention_hours);
        anyhow::ensure!(self.maintenance_secs > 0, "map.maintenance_secs={} must be positive", self.maintenance_secs);
        super::game::duration("map.history_retention_hours", self.history_retention_hours, infinity_core::consts::FILETIME_HOUR, false)?;
        super::game::duration("map.maintenance_secs", self.maintenance_secs, infinity_core::consts::FILETIME_SECOND, true)
    }
}
