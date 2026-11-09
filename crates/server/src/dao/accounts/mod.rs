//! Account credentials, clone-grade authority, creation date and PLEX balance persistence.
//! Clone-grade authority mirrors `omega/client/clone_grade_svc.py`; other account data has no direct client mirror.
//! Data is stored in `game.accounts`, `game.account_clone_grades` and `game.account_clone_grade_audit`.
//! Authentication and session policy, transactions, refusals, notifications and World are caller concerns.

// Both account creation paths initialize the same unlimited Omega authority.
macro_rules! initial_authority_sql {
    ($prefix:literal, $suffix:literal) => {
        concat!($prefix, "authority AS (INSERT INTO game.account_clone_grades (user_id, grade) SELECT user_id, 1 FROM account ON CONFLICT (user_id) DO NOTHING) ", $suffix)
    };
}

pub(crate) mod authentication;
pub(crate) mod subscriptions;
pub(crate) mod balances;
pub(crate) mod plex;
pub(crate) mod selection;
pub(crate) mod simulated;
