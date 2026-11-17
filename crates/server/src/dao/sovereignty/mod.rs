//! Typed sovereignty persistence; client behavior and transactions belong to callers.
//! Uses the game sovereignty tables and authored Lua package versions.
//! Development-index points in game.sov_development* are independent of Hub or
//! owner rows, so losing a Hub or a claim never removes index state.

pub(crate) mod systems;
pub(crate) mod structures;
pub(crate) mod capture;
pub(crate) mod policy;
pub(crate) mod history;
pub(crate) mod sources;
pub(crate) mod development;

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
