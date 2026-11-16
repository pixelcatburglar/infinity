//! Shared synchronous-lock policy: a poisoned `std::sync` lock means a thread
//! panicked while holding shared state, so what the lock protected can no
//! longer be trusted. The next consumer of the poison does not recover the
//! inner value and does not turn it into a request error; it reports once and
//! the process exits. Panics that never touch a shared lock keep their
//! existing isolation. No client protocol mirror, no tables.

use std::io::Write;

use crate::space::containment::ContainmentFailure;

/// Exit classification for losing shared synchronous state to a poisoned
/// lock; the signal handed to the supervising process, not a tunable.
const STATE_EXIT_CODE: i32 = 1;

/// Returns the lock payload when the lock is healthy. A poisoned lock means a
/// holder panicked with its guard out, so the protected state is untrusted:
/// report and exit rather than propagate, recover or wrap the failure.
#[track_caller]
#[inline]
pub(crate) fn lock_or_exit<T>(result: std::sync::LockResult<T>) -> T {
    match result {
        Ok(value) => value,
        // The error carries the untrusted payload; forget it so its Drop never
        // runs on damaged state before the exit.
        Err(error) => {
            std::mem::forget(error);
            poisoned(std::panic::Location::caller());
        }
    }
}

#[cold]
fn poisoned(at: &'static std::panic::Location<'static>) -> ! {
    fatal("shared lock poisoned", at, None);
}

/// A closed containment proof failed. Callers release every application lock
/// before calling; this reports and exits without reading another one.
#[track_caller]
#[cold]
pub(crate) fn containment_lost(reason: ContainmentFailure, system: i64, character: Option<i64>) -> ! {
    fatal(&reason.to_string(), std::panic::Location::caller(), Some((system, character)));
}

#[cold]
fn fatal(reason: &str, at: &'static std::panic::Location<'static>, identity: Option<(i64, Option<i64>)>) -> ! {
    let identity = identity.map(|(system, character)| format!(" system {system} character {character:?}")).unwrap_or_default();
    {
        let mut out = std::io::stderr().lock();
        let _ = writeln!(out, "FATAL {reason} at {at}{identity} commit {} exit {STATE_EXIT_CODE}", crate::build_identity::commit());
        let _ = out.flush();
    }
    if !std::thread::panicking() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tracing::error!(location = %at, commit = crate::build_identity::commit(), exit_code = STATE_EXIT_CODE, reason, "shared state untrusted");
        }));
    }
    std::process::exit(STATE_EXIT_CODE);
}

#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
