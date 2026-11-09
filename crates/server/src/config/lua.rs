//! Lua runtime configuration and per-script operator settings.
//! Scripts provide their own client-facing behavior; no client module is mirrored here.
//! Settings come from operator TOML, not the database. Runtime reload is unsupported.

use std::collections::HashMap;
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LuaConfig {
    pub enabled: bool,
    pub scripts_dir: String,
    pub mod_dirs: Vec<String>,
    /// Optional external published adaptation content root.
    pub content_data_dir: Option<String>,
    pub memory_limit_mb: usize,
    pub call_budget_ms: u64,
    pub call_budget_instructions: u64,
    pub instruction_hook_interval: u32,
    pub store_value_limit_bytes: usize,
    /// Server policy: maximum bytes read for one authored Lua data snapshot; the measured 340636-byte snapshot is roughly twelve times smaller.
    pub data_file_limit_bytes: usize,
    pub scripts: HashMap<String, toml::Table>,
}

impl Default for LuaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scripts_dir: "crates/server/lua".into(),
            mod_dirs: Vec::new(),
            content_data_dir: None,
            memory_limit_mb: 64,
            // Allow the slowest simulated pilot operation, bringing a pilot online, with headroom.
            call_budget_ms: 1000,
            call_budget_instructions: 5_000_000,
            instruction_hook_interval: 1000,
            store_value_limit_bytes: 65536,
            data_file_limit_bytes: 4 * 1024 * 1024,
            scripts: HashMap::new(),
        }
    }
}

impl LuaConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.scripts_dir.is_empty(), "lua.scripts_dir must not be empty");
        anyhow::ensure!(self.mod_dirs.iter().all(|dir| !dir.is_empty()), "lua.mod_dirs entries must not be empty");
        anyhow::ensure!(self.content_data_dir.as_ref().is_none_or(|dir| !dir.is_empty()), "lua.content_data_dir must not be empty");
        anyhow::ensure!(self.memory_limit_mb > 0 && self.memory_limit_mb.checked_mul(1024 * 1024).is_some(), "lua.memory_limit_mb={} is zero or overflows bytes", self.memory_limit_mb);
        anyhow::ensure!(self.call_budget_ms > 0 && std::time::Instant::now().checked_add(std::time::Duration::from_millis(self.call_budget_ms)).is_some(), "lua.call_budget_ms={} is zero or overflows the deadline", self.call_budget_ms);
        anyhow::ensure!(self.call_budget_instructions > 0, "lua.call_budget_instructions must be nonzero");
        anyhow::ensure!(self.instruction_hook_interval > 0, "lua.instruction_hook_interval must be nonzero");
        anyhow::ensure!(u64::from(self.instruction_hook_interval) <= self.call_budget_instructions, "lua.instruction_hook_interval exceeds lua.call_budget_instructions");
        anyhow::ensure!(self.store_value_limit_bytes > 0, "lua.store_value_limit_bytes must be nonzero");
        anyhow::ensure!(self.data_file_limit_bytes > 0, "lua.data_file_limit_bytes must be nonzero");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
