//! Shared skill level reads, presence, training, queue, plan and expert-system lifecycle
//! persistence using `game.skills`, `game.skill_queue`, `game.skill_plans` and
//! `game.character_expert_systems`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Training and expert-system policy, transactions, refusals, notifications and World are
//! caller concerns.

pub(crate) mod alpha_injections;
pub(crate) mod expert_systems;
pub(crate) mod history;
pub(crate) mod levels;
pub(crate) mod plans;
pub(crate) mod queue;
pub(crate) mod selection;
pub(crate) mod training;
pub(crate) mod trading;
