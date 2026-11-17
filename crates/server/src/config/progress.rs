//! Durable progress receipt retry policy, sourced from server defaults and
//! operator TOML. Only the permanent-rejection budget is configurable; the
//! retry backoff and drain batch bound are server constants of `progress/`.

use anyhow::Result;
use serde::Deserialize;

/// Server operational bound: allow at most 100 retries of a permanently
/// rejected receipt while keeping its capped rejection counter small.
const MAX_PERMANENT_RETRY_LIMIT: i32 = 100;

/// Retry policy for durable progress receipts.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProgressConfig {
    /// Server policy: explicit permanent rejections tolerated before the next
    /// one turns the receipt terminal; default 5.
    pub permanent_retry_limit: i32,
}

impl Default for ProgressConfig {
    fn default() -> Self {
        Self { permanent_retry_limit: 5 }
    }
}

impl ProgressConfig {
    pub(super) fn validate(&self) -> Result<()> {
        anyhow::ensure!((1..=MAX_PERMANENT_RETRY_LIMIT).contains(&self.permanent_retry_limit),
            "progress.permanent_retry_limit={} must be between 1 and {MAX_PERMANENT_RETRY_LIMIT}", self.permanent_retry_limit);
        Ok(())
    }
}

#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
