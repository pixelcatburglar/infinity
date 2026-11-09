//! Player-visible PLEX vault balance refusals for inventory conversions.
//! The client conversion flow is in `plexVault.py` and `sellMulti.py`; these
//! texts use the
//! existing `CustomNotify` catalogue key. This module reads no data and does not
//! implement vault mutations or localisation.

/// A withdrawal exceeds the available PLEX vault balance.
pub const INSUFFICIENT_BALANCE: &str = "PLEX 仓库余额不足。";
/// A credit would exceed the PLEX vault's numeric maximum.
pub const BALANCE_LIMIT_EXCEEDED: &str = "PLEX 仓库余额超过上限。";
