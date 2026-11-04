//! Effective skill snapshots combine persisted training with active expert grants.
use crate::dao::skills::{expert_systems::ExpertSystem as Installation, training::Skill};
use crate::db::executor::Executor;
use crate::services::{db_err, CallError};
use crate::sde::Sde;
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

/// Read and project the rows active at one captured FILETIME.
pub(crate) async fn snapshot_at(
    ex: &(impl Executor + ?Sized),
    sde: &Sde,
    character: i64,
    at: i64,
) -> Result<SkillSnapshot, CallError> {
    let (trained, installations) = tokio::try_join!(
        crate::dao::skills::training::current(ex, character),
        crate::dao::skills::expert_systems::active(ex, character, at),
    )
    .map_err(db_err)?;
    Ok(project_core(&trained, &installations, |type_id| {
        sde.expert_systems.get(type_id).map(|definition| definition.skills.as_slice())
    }, at))
}

/// Project persisted rows using a caller-provided expert-system definition lookup.
fn project_core<'a, Lookup>(
    trained: &[Skill],
    installations: &[Installation],
    lookup: Lookup,
    at: i64,
) -> SkillSnapshot
where
    Lookup: Fn(i32) -> Option<&'a [(i32, i32)]>,
{
    let mut entries: BTreeMap<i32, SkillEntry> = trained
        .iter()
        .cloned()
        .map(|skill| (skill.type_id, SkillEntry { trained: Some(skill), virtual_level: None }))
        .collect();
    for installation in installations {
        if installation.installed_at > at || at >= installation.expires_at {
            continue;
        }
        let Some(skills) = lookup(installation.type_id) else { continue };
        for &(type_id, level) in skills {
            let entry = entries.entry(type_id).or_insert_with(|| SkillEntry {
                trained: None,
                virtual_level: None,
            });
            entry.virtual_level = Some(entry.virtual_level.unwrap_or(0).max(level));
        }
    }
    SkillSnapshot { entries }
}

/// Project persisted rows and grants without querying or cloning static data.
pub(crate) fn project(
    trained: &[Skill],
    installations: &[Installation],
    sde: &Sde,
    at: i64,
) -> SkillSnapshot {
    project_core(&trained, &installations, |type_id| {
        sde.expert_systems.get(type_id).map(|definition| definition.skills.as_slice())
    }, at)
}

#[cfg(test)]
mod tests;
