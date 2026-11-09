//! Runtime resource limits and QA allowlist for daily goals.
//! The client history batch is specified by dailygoals/client/const.py::BATCH_SIZE.
//! This policy does not configure content, rewards, or release availability.

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashSet;

/// Client history request size, from dailygoals/client/const.py::BATCH_SIZE.
const CLIENT_BATCH_SIZE: u32 = 25;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DailyGoalsConfig {
    /// Server runtime policy: maximum history rows per request.
    pub history_page_maximum: u32,
    /// Server runtime policy: maximum characters processed per rollover batch.
    pub rollover_batch_size: usize,
    /// Server runtime policy: accounts eligible for QA, closed by default.
    pub qa_account_ids: Vec<i64>,
}

impl Default for DailyGoalsConfig {
    fn default() -> Self {
        Self { history_page_maximum: 100, rollover_batch_size: 64, qa_account_ids: Vec::new() }
    }
}

impl DailyGoalsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.history_page_maximum >= CLIENT_BATCH_SIZE, "game.daily_goals.history_page_maximum must be at least {CLIENT_BATCH_SIZE}");
        anyhow::ensure!(self.rollover_batch_size > 0, "game.daily_goals.rollover_batch_size must be positive");
        let mut seen = HashSet::new();
        anyhow::ensure!(self.qa_account_ids.iter().all(|id| *id > 0 && seen.insert(*id)), "game.daily_goals.qa_account_ids must contain unique positive account IDs");
        Ok(())
    }
}
