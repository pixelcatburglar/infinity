//! Server-only panic reporting: one structured ERROR event per panic, then the
//! previous hook runs unchanged. The backtrace is force-captured only when a
//! panic fires; nothing is captured on the normal path. The commit field is
//! the compile-time build identity, not a runtime or database read. This is
//! reporting, not recovery: no catch, abort, exit or restart is added, and no
//! client module or database is involved.
use std::backtrace::Backtrace;

/// Chains the reporting hook onto the previously installed hook (normally the
/// standard library's, which keeps its stderr output). Call once after logging
/// is initialized; installing emits no normal-path log line.
pub(crate) fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let thread = std::thread::current();
        let backtrace = Backtrace::force_capture();
        tracing::error!(
            location = ?panic.location(),
            message = panic.payload_as_str().unwrap_or("non-string panic payload"),
            thread = ?thread.name(),
            commit = crate::build_identity::commit(),
            backtrace = %backtrace,
        );
        previous(panic);
    }));
}

#[cfg(all(test, feature = "tests-runtime"))]
mod tests;
