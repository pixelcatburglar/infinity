//! Expert-system feature policy loaded from operator TOML.
//! The flag is advertised through the machoNet global configuration; expert-system
//! RPC, GM commands and expiry processing are intentionally not gated here.

use serde::Deserialize;

/// Operator toggle for advertising expert-system support to clients.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExpertSystemsConfig {
    /// Server policy: advertise the expert-system feature to the client.
    pub enabled: bool,
}

impl Default for ExpertSystemsConfig {
    fn default() -> Self { Self { enabled: true } }
}
