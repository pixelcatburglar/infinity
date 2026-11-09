//! Player-visible refusal for closed brawl operations.
//! Mirrors `brawl/client/brawl_svc.py` and
//! `brawl/client/brawl_novice_guide_controller.py`. The brawl client displays
//! this through CustomNotify; no data source is used.
//! Brawl gameplay is not implemented.

/// Brawl operations are unavailable.
pub const UNAVAILABLE: &str = "乱斗玩法暂不可用。";
