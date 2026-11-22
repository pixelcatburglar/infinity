//! Temporary probe-ball IDs shared by scanning launches and interdiction probe
//! launches. One counter keeps both producers inside the same temporary range;
//! the IDs never reach `game.items` and are not restored after restart.
//! No client analogue; the range is a server design choice.

use std::sync::atomic::{AtomicI64, Ordering};

/// Start of the temporary probe ID range (server design).
const PROBE_ID_START: i64 = 9_600_000_000_000;

/// The one temporary probe counter shared by every producer.
static NEXT_PROBE_ID: AtomicI64 = AtomicI64::new(PROBE_ID_START);

/// Allocate one temporary probe entity ID.
pub(crate) fn next_probe_id() -> i64 {
    NEXT_PROBE_ID.fetch_add(1, Ordering::Relaxed)
}
