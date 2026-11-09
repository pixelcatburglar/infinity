//! Player-visible refusals for unsupported player structures.
//! Mirrors action calls in `eve/client/script/ui/services/structure/`;
//! these texts read no database or other data source and do not implement gameplay.

pub const UNAVAILABLE: &str = "玩家建筑目前不可用。";
pub const STARBASE_UNAVAILABLE: &str = "星基地目前不可用。";
