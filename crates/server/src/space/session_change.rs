//! The shared ship-change deadline, one record per live pilot in the
//! notification registry.
//!
//! The client applies `SESSIONCHANGEDELAY`
//! (carbon/common/script/sys/basesession.py) before every PerformSessionChange
//! Ship changes: Board docked or in space, StoreVessel, BoardStoredShip and
//! Eject. Launching a stored hull is not a ship change, and neither is
//! refreshing the already-selected one. The registry enforces the same wait
//! itself because the client delay is advisory: a genuinely new live
//! registration arms the interval. A repeated callback on the same channel
//! changes nothing. Replacing the channel keeps the later of the running
//! deadline and the replacement's own interval, and clears the session
//! snapshot. A committed change records the deadline frozen at its admission.
//! Refused or uncommitted changes never arm or refresh the deadline, and a
//! stale session generation cannot write another session's record. The record
//! Its lifetime follows the live registration, with no timer, queue, tombstone or
//! persistence; a restarted process has no live pilots and nothing to fence.

use super::*;

/// `basesession.py` SESSIONCHANGEDELAY, in whole seconds.
const SHIP_CHANGE_DELAY_SECONDS: i64 = 10;

/// The deadline a ship change admitted at FILETIME `now` records once it
/// commits.
pub(crate) fn ship_change_deadline(now: i64) -> i64 {
    crate::client::filetime_deadline_from_seconds_saturating(now, SHIP_CHANGE_DELAY_SECONDS)
}

/// One live pilot's notification channel, live session snapshot and the
/// earliest FILETIME its next ship change may be admitted.
pub(super) struct Registration {
    pub(super) tx: NotifyTx,
    pub(super) session: Option<infinity_core::session::OnlineSession>,
    pub(super) ready_at: i64,
}

impl Pilots {
    /// Register or replace a pilot's notification channel. A genuinely new
    /// live character arms the client's ship-change interval from the current
    /// FILETIME. The same channel again is a duplicate callback and changes
    /// neither the deadline nor the session. A different channel is a
    /// replacement: it keeps the later deadline and clears the session
    /// snapshot, exactly as a fresh notification channel requires.
    pub fn register_pilot(&self, char_id: i64, tx: NotifyTx) {
        self.register_pilot_clock(char_id, tx, crate::client::filetime_now());
    }

    fn register_pilot_clock(&self, char_id: i64, tx: NotifyTx, now: i64) {
        let mut notify = crate::poison::lock_or_exit(self.notify.lock());
        match notify.get_mut(&char_id) {
            Some(entry) if entry.tx.same_channel(&tx) => {}
            Some(entry) => {
                entry.tx = tx;
                entry.session = None;
                let fresh = ship_change_deadline(now);
                if fresh > entry.ready_at { entry.ready_at = fresh; }
            }
            None => {
                notify.insert(char_id, Registration { tx, session: None, ready_at: ship_change_deadline(now) });
            }
        }
    }

    /// Whether `character`'s live session `session_id` may start a ship change
    /// at FILETIME `now`: the channel is open, the generation matches and the
    /// recorded deadline has elapsed.
    pub(crate) fn ship_change_ready(&self, character: i64, session_id: i64, now: i64) -> bool {
        crate::poison::lock_or_exit(self.notify.lock()).get(&character).is_some_and(|entry| {
            !entry.tx.is_closed()
                && entry.session.as_ref().is_some_and(|live| live.character_id == character && live.session_id == session_id)
                && entry.ready_at <= now
        })
    }

    /// Record the deadline frozen at the admission of a change this session
    /// actually committed. A later frozen deadline replaces an earlier one;
    /// replaying the same or an older value leaves the record unchanged.
    /// Returns false when the live generation does not match: a stale session
    /// cannot write another session's deadline, and a refusal or an unknown
    /// outcome records nothing.
    pub(crate) fn commit_ship_change(&self, character: i64, session_id: i64, ready_at: i64) -> bool {
        let mut notify = crate::poison::lock_or_exit(self.notify.lock());
        let Some(entry) = notify.get_mut(&character) else { return false };
        if entry.tx.is_closed()
            || entry.session.as_ref().is_none_or(|live| live.character_id != character || live.session_id != session_id) {
            return false;
        }
        if ready_at > entry.ready_at { entry.ready_at = ready_at; }
        true
    }
}

impl system::System {
    /// Actor-turn readiness check against the registry this actor already
    /// holds. The turn does not upgrade or call `World`.
    pub(crate) fn ship_change_ready(&self, character: i64, session_id: i64, now: i64) -> bool {
        self.registry.ship_change_ready(character, session_id, now)
    }
}

impl World {
    /// Readiness check outside an actor turn (the station ship-change path).
    pub(crate) fn ship_change_ready(&self, character: i64, session_id: i64, now: i64) -> bool {
        self.pilots.ship_change_ready(character, session_id, now)
    }

    /// Record the frozen deadline of a change `session_id` actually committed.
    pub(crate) fn commit_ship_change(&self, character: i64, session_id: i64, ready_at: i64) -> bool {
        self.pilots.commit_ship_change(character, session_id, ready_at)
    }
}

#[cfg(all(test, any(feature = "tests-space", feature = "tests-ships")))]
#[path = "session_change/test_support.rs"]
pub(crate) mod test_support;

#[cfg(all(test, feature = "tests-space"))]
#[path = "session_change/tests.rs"]
mod tests;
