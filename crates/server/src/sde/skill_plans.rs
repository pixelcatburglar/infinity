//! Certified plans and native prerequisite graphs loaded at startup.
//! The client tracks certified plans by static numeric ID and reads required skills for types.
//! Personal plan contents and mutable progress are stored separately. Cyclic native
//! edges are retained and roots reaching cycles are indexed for validation.
use std::collections::{HashMap, HashSet};
use crate::{dao::sde::skill_plans as dao, db::executor::Executor};
use super::types::Types;

#[derive(Default)]
pub(crate) struct SkillPlans { ids: HashSet<u32>, requirements: HashMap<i32, Vec<(i32, i32)>>, cyclic: HashSet<i32> }
impl SkillPlans {
    pub(crate) async fn load(ex: &(impl Executor + ?Sized), types: &Types) -> anyhow::Result<Self> {
        Self::from_rows(dao::load(ex).await?, dao::requirements(ex).await?, |id| types.get(id).is_some())
    }
    fn from_rows(plan_rows: Vec<dao::SkillPlanRow>, requirement_rows: Vec<dao::SkillPlanRow>, type_exists: impl Fn(i32) -> bool) -> anyhow::Result<Self> {
        let mut ids = HashSet::new();
        for row in plan_rows {
            let key: u32 = row.key.as_deref().ok_or_else(|| anyhow::anyhow!("certified skill plan missing key"))?.parse()?;
            let data: serde_json::Value = serde_json::from_str(&row.data)?;
            anyhow::ensure!(key != 0 && data.get("_key").and_then(|v| v.as_u64()) == Some(u64::from(key)), "invalid certified skill plan key {key}");
            anyhow::ensure!(ids.insert(key), "duplicate certified skill plan key {key}");
        }
        let mut requirements = HashMap::new();
        for row in requirement_rows {
            let key: i32 = row.key.as_deref().ok_or_else(|| anyhow::anyhow!("skill prerequisites missing key"))?.parse()?;
            let data: serde_json::Value = serde_json::from_str(&row.data)?;
            let fields = data.as_object().ok_or_else(|| anyhow::anyhow!("invalid prerequisites for type {key}"))?;
            anyhow::ensure!(key > 0 && fields.get("_key").and_then(|v| v.as_i64()) == Some(i64::from(key)), "invalid prerequisite key {key}");
            let mut skills = Vec::with_capacity(fields.len().saturating_sub(1));
            for (id, level) in fields.iter().filter(|(id, _)| id.as_str() != "_key") {
                let id: i32 = id.parse()?;
                let level = level.as_i64().and_then(|value| i32::try_from(value).ok()).ok_or_else(|| anyhow::anyhow!("invalid prerequisite level for type {key}, skill {id}"))?;
                anyhow::ensure!(id > 0 && (0..=infinity_core::consts::dogma::MAX_SKILL_LEVEL).contains(&level) && type_exists(id), "invalid prerequisite for type {key}, skill {id}, level {level}");
                skills.push((id, level));
            }
            skills.sort_unstable();
            anyhow::ensure!(requirements.insert(key, skills).is_none(), "duplicate prerequisites for type {key}");
        }
        let mut remaining: HashMap<i32, usize> = requirements.iter().map(|(&id, edges)| (id, edges.len())).collect();
        let mut parents: HashMap<i32, Vec<i32>> = HashMap::new();
        for (&id, edges) in &requirements {
            for &(child, _) in edges {
                remaining.entry(child).or_insert(0);
                parents.entry(child).or_default().push(id);
            }
        }
        let mut leaves: Vec<i32> = remaining.iter().filter_map(|(&id, &count)| (count == 0).then_some(id)).collect();
        while let Some(child) = leaves.pop() {
            if let Some(parents) = parents.get(&child) {
                for parent in parents {
                    let count = remaining.get_mut(parent).expect("parent in graph");
                    *count -= 1;
                    if *count == 0 { leaves.push(*parent); }
                }
            }
        }
        let cyclic = remaining.into_iter().filter_map(|(id, count)| (count != 0).then_some(id)).collect();
        Ok(Self { ids, requirements, cyclic })
    }
    pub(crate) fn contains(&self, id: u32) -> bool { self.ids.contains(&id) }
    pub(crate) fn requirements(&self, id: i32) -> &[(i32, i32)] { self.requirements.get(&id).map_or(&[], Vec::as_slice) }
    pub(crate) fn is_cyclic(&self, id: i32) -> bool { self.cyclic.contains(&id) }
}

#[cfg(test)]
mod tests;
