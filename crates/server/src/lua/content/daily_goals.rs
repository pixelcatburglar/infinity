//! Immutable authored daily-goal definitions and closed policy operations.
//! Client presentation consumes `dailygoals/client/utils.py`; content is server-authored.
//! Gateway handlers and progress settlement are supplied by their owning integrations.
pub(crate) mod model;
pub(crate) mod plans;

#[cfg(test)] mod tests;
