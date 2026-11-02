//! Account credentials, creation date and PLEX balance persistence in `game.accounts`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Authentication and session policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod authentication;
pub(crate) mod balances;
pub(crate) mod selection;
pub(crate) mod simulated;
