//! Native SDE skill prerequisites shared by training, fitting and skill plans.
//! Mirrors requiredskillsfortypes, loaded once at startup, including level-zero entries.
//! Transactions and prerequisite enforcement are caller concerns.
use crate::sde::Sde;

/// Required type and level pairs for a type.
pub(crate) fn requirements(sde: &Sde, type_id: i32) -> impl Iterator<Item = (i32, i32)> + '_ {
    sde.skill_plans.requirements(type_id).iter().copied()
}

#[cfg(test)] mod tests;
