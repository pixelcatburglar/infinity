//! Shared skill level reads, presence, training, queue and plan persistence using `game.skills`,
//! `game.skill_queue` and `game.skill_plans`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Training policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod alpha_injections;
pub(crate) mod history;
pub(crate) mod levels;
pub(crate) mod plans;
pub(crate) mod queue;
pub(crate) mod selection;
pub(crate) mod training;
pub(crate) mod trading;
