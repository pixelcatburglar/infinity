//! Provides target visibility checks and generations for space actors.
//! No direct client mirror; the combat service owns the targeting wire contract.
//! No tables; uses ball state and an in-memory counter.
//! Visibility alone does not check range, locks or targeting eligibility.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TARGET_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_generation() -> u64 {
    NEXT_TARGET_GENERATION.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("target generation exhausted")
}

pub(super) fn target_visible(target: &Ball) -> bool {
    target.cloak == 0 && target.mode != MODE_WARP
}
