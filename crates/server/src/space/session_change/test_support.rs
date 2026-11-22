//! Fixture registration at a past FILETIME, so its ten-second ship-change
//! deadline has already elapsed. A replacement does not shorten a later
//! deadline. This is not a production registration and it does not record a
//! committed change.

use super::*;

/// Register `character` on `tx` at FILETIME 0. The armed deadline is the
/// client interval after that instant, which admission's current clock has
/// passed. The caller sets the live session afterwards.
pub(crate) fn register_elapsed(world: &World, character: i64, tx: NotifyTx) {
    world.pilots.register_pilot_clock(character, tx, 0);
}

/// Whether `error` is the shared ship-change deadline refusal.
#[cfg(feature = "tests-ships")]
pub(crate) fn ship_change_cooldown(error: &crate::services::CallError) -> bool {
    matches!(error, crate::services::CallError::User { msg, dict }
        if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY
            && dict.iter().any(|(key, value)| key == "notify"
                && matches!(value, Value::Unicode(text) if text == infinity_core::messages::ship::SHIP_CHANGE_NOT_READY)))
}

/// Wait out one client ship-change interval. The committed deadline is left
/// as the change recorded it.
#[cfg(feature = "tests-ships")]
pub(crate) async fn wait_for_ship_change() {
    tokio::time::sleep(std::time::Duration::from_secs(SHIP_CHANGE_DELAY_SECONDS as u64)).await;
}
