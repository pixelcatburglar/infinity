//! Server resource policy for shared-setting encoding, not a client mirror.
//! `sharedSettings/util.py:GetDataFromSettingKey` consumes the encoded payload.
//! Defaults are server choices supplied through game configuration; per-client
//! negotiation is unsupported and depth is limited by the recursive encoder.
use serde::Deserialize;

/// Server implementation ceiling for recursive logical Value-tree depth.
const MAX_IMPLEMENTED_DEPTH: usize = 64;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SharedSettingsConfig {
    /// Maximum encoded UTF-8 payload size in bytes; default is a server choice.
    pub max_payload_bytes: usize,
    /// Maximum logical Value-tree depth; defaults to the implementation ceiling.
    pub max_nesting_depth: usize,
}

impl Default for SharedSettingsConfig {
    fn default() -> Self { Self { max_payload_bytes: 262144, max_nesting_depth: MAX_IMPLEMENTED_DEPTH } }
}

impl SharedSettingsConfig {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.max_payload_bytes > 0, "shared_settings.max_payload_bytes must be positive");
        anyhow::ensure!((1..=MAX_IMPLEMENTED_DEPTH).contains(&self.max_nesting_depth), "shared_settings.max_nesting_depth must be between 1 and {MAX_IMPLEMENTED_DEPTH}");
        Ok(())
    }
}
