//! Starting skill grants for character creation, based on SDE race skills.
//! No specific client module path is established for these server-side grants.
//! Reads startup SDE race skills and type attributes and writes game.skills.
//! Other character creation is handled elsewhere.
use super::{db_err, CallError};
use super::formulas::sp_for_level;
use super::training::skill_type;
use crate::{db::executor::Executor, sde::Sde};

/// Grant the race's starting skills to a new character (sde chr_races.skills).
pub(crate) async fn grant_starting_skills(ex: &(impl Executor + ?Sized), sde: &Sde, char_id: i64, race_id: i32) -> Result<(), CallError> {
    for (type_id, level) in starting_levels(sde, race_id) {
        let rank = skill_type(sde, type_id).rank;
        crate::dao::skills::training::starting_skill(ex, char_id, type_id, level, sp_for_level(rank, level as i64)).await.map_err(db_err)?;
    }
    Ok(())
}

pub(super) fn starting_levels(sde: &crate::sde::Sde, race_id: i32) -> impl Iterator<Item = (i32, i32)> + '_ {
    sde.characters.race(race_id as i64).into_iter().flat_map(|race| &race.skills)
        .map(|skill| (skill.type_id, skill.level))
}

#[cfg(test)]
mod tests;
