//! Skill snapshots for gameplay consumers preserve persisted training rows and
//! separate non-trained level contributions. Reads `game.skills` through the
//! training DAO; mirrors CharacterSkillEntry effective-level accessors.
//! Only trained rows are currently loaded.
use crate::dao::skills::training::Skill;
use crate::db::executor::Executor;
use crate::services::{db_err, CallError};
use std::collections::BTreeMap;

/// A persisted skill and its optional non-trained level contribution.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SkillEntry {
    pub(crate) trained: Option<Skill>,
    pub(crate) virtual_level: Option<i32>,
}

impl SkillEntry {
    pub(crate) fn effective_level(&self) -> i32 {
        self.trained.as_ref().map_or(0, |skill| skill.level)
            .max(self.virtual_level.unwrap_or(0))
    }
}

/// Skills observed at one point in time, ordered by type ID.
pub(crate) struct SkillSnapshot {
    entries: BTreeMap<i32, SkillEntry>,
}

impl SkillSnapshot {
    pub(crate) fn get(&self, type_id: i32) -> Option<&SkillEntry> {
        self.entries.get(&type_id)
    }

    pub(crate) fn level(&self, type_id: i32) -> i32 {
        self.get(type_id).map_or(0, SkillEntry::effective_level)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (i32, &SkillEntry)> {
        self.entries.iter().map(|(&type_id, entry)| (type_id, entry))
    }
}

/// Read persisted training rows for one character.
pub(crate) async fn snapshot(
    ex: &(impl Executor + ?Sized),
    char_id: i64,
) -> Result<SkillSnapshot, CallError> {
    let skills = crate::dao::skills::training::current(ex, char_id)
        .await
        .map_err(db_err)?;
    Ok(from_owned(skills.into_iter()))
}

/// Build a snapshot from an already captured set of persisted training rows.
pub(crate) fn from_trained(skills: &[Skill]) -> SkillSnapshot {
    from_owned(skills.iter().cloned())
}

fn from_owned(skills: impl Iterator<Item = Skill>) -> SkillSnapshot {
    let entries = skills.map(|skill| {
        (skill.type_id, SkillEntry { trained: Some(skill), virtual_level: None })
    }).collect();
    SkillSnapshot { entries }
}

#[cfg(test)]
mod tests;
