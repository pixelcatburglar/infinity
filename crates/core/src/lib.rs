//! Shared game constants, server-authored player text, and distributed session data.
//! Client mirrors and value sources are documented on `consts`, `messages`, and `session`.
//! Callers supply marshal values. This crate has no database, runtime configuration,
//! clock, identity authority, network transport, or game-service implementation.

pub mod consts;
pub mod messages;
pub mod session;
