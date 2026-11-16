//! Operator policy for GM-created space objects and performance testing.
use anyhow::Result;
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GmConfig {
    /// Server policy: default distance for GM spawns, in metres.
    pub spawn_distance_m: f64,
    /// Server policy: default radius for clearing GM spawns, in metres.
    pub clear_radius_m: f64,
    /// Server policy: maximum number of NPCs in one GM spawn operation.
    pub max_spawn_count: usize,
    /// Server policy: authorize explicit full-system performance-testing administration commands.
    pub performance_testing: bool,
}
impl Default for GmConfig {
    fn default() -> Self { Self { spawn_distance_m: 5000.0, clear_radius_m: 100000.0, max_spawn_count: 100, performance_testing: false } }
}
impl GmConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.spawn_distance_m.is_finite() && self.spawn_distance_m >= 0.0, "gm.spawn_distance_m must be finite and nonnegative");
        anyhow::ensure!(self.clear_radius_m.is_finite() && self.clear_radius_m >= 0.0, "gm.clear_radius_m must be finite and nonnegative");
        anyhow::ensure!(self.max_spawn_count > 0, "gm.max_spawn_count must be positive");
        Ok(())
    }
}
