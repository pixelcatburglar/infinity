//! Player-visible New Eden Store redeem errors.
//! Mirrors redeemsvc.py; no database access or localisation lookup is performed.

/// A redeem token could not be delivered.
pub(crate) const REDEEM_FAILED: &str = "无法领取此物品，请稍后重试。";
