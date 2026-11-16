//! Compile-time build identity: the package version and the commit stamped by
//! the build environment, both fixed strings in the binary; no runtime read,
//! caching or allocation; without the variable the commit is `dev`.
//! Server-only; mirrors no client module and reads no game data.

/// The server package version, fixed at compile time.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit stamped by `INFINITY_BUILD_COMMIT` at compile time, or `dev`.
pub(crate) fn commit() -> &'static str {
    option_env!("INFINITY_BUILD_COMMIT").unwrap_or("dev")
}

/// Reports the build identity once, right after logging is initialized.
pub(crate) fn log_startup() {
    tracing::info!(version = VERSION, commit = commit(), "server build");
}
