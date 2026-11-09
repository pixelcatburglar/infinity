//! Persistence for frozen daily goal assignments and receipts.
//! Client daily-goal projections are owned by the service, not this DAO.
//! Reads and writes game.daily_goal_* and retained Lua package references;
//! transactions, economy effects, scheduling and policy are deliberately excluded.

pub(crate) mod periods;
pub(crate) mod instances;
pub(crate) mod rewards;
pub(crate) mod receipts;

#[cfg(test)]
mod tests;
