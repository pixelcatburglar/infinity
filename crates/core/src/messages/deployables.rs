//! Player-visible text for launched fields and deployables.
//! The client displays these through `CustomNotify`; this module holds no
//! catalogue keys of its own and reads no data.

/// A probe launch was refused after admission without spending a charge.
pub const PROBE_LAUNCH_REFUSED: &str = "探针发射被拒绝，弹药未消耗。";
