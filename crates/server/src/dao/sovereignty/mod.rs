//! Typed Hub persistence; client behavior and transactions belong to callers.
//! Uses the game sovereignty tables and authored Lua package versions.

pub(crate) mod systems;
pub(crate) mod structures;
pub(crate) mod capture;
pub(crate) mod policy;
pub(crate) mod history;
pub(crate) mod sources;

#[cfg(test)]
mod tests;
