//! Player bounty and kill-right timing policy.
//! Defaults are server policy; persisted deadlines use FILETIME ticks.
use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BountyConfig {
    /// Server policy: lifetime of an earned kill right in days.
    pub kill_right_lifetime_days: i64,
    /// Server policy: suspect duration after activating a kill right in seconds.
    pub kill_right_suspect_secs: i64,
}

impl Default for BountyConfig {
    fn default() -> Self {
        Self { kill_right_lifetime_days: 30, kill_right_suspect_secs: 900 }
    }
}

impl BountyConfig {
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.kill_right_lifetime_days > 0, "bounty.kill_right_lifetime_days={} must be positive", self.kill_right_lifetime_days);
        anyhow::ensure!(self.kill_right_suspect_secs > 0, "bounty.kill_right_suspect_secs={} must be positive", self.kill_right_suspect_secs);
        super::game::duration("bounty.kill_right_lifetime_days", self.kill_right_lifetime_days, infinity_core::consts::FILETIME_DAY, true)?;
        super::game::duration("bounty.kill_right_suspect_secs", self.kill_right_suspect_secs, infinity_core::consts::FILETIME_SECOND, true)
    }
}
