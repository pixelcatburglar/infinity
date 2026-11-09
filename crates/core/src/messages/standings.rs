//! Standings text: the refusal of an absolute standing outside the client's
//! -10 to 10 range (`standings::set`). Reads no tables and validates nothing.

/// Refusal of a non-finite standing or one outside -10..=10.
pub const STANDING_OUT_OF_RANGE: &str = "声望必须是 -10 到 10 之间的有限数值。";
