//! Account credentials, clone-grade authority, creation date and PLEX balance persistence.
//! Clone-grade authority mirrors `omega/client/clone_grade_svc.py`; other account data has no direct client mirror.
//! Data is stored in `game.accounts`, `game.account_clone_grades` and `game.account_clone_grade_audit`.
//! Authentication and session policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod authentication;
pub(crate) mod subscriptions;
pub(crate) mod balances;
pub(crate) mod selection;
pub(crate) mod simulated;
