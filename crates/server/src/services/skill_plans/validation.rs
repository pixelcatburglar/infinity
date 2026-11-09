//! Shared personal skill-plan and milestone rules.
//! Mirrors skillPlanValidation.py and milestoneController.py using startup SDE prerequisites.
//! No expert-system effective levels are used in milestone completion. Cyclic
//! prerequisite closures cannot be used for new steps or targets.
use std::collections::{BTreeMap, HashSet};
use infinity_core::{consts::{CATEGORY_SKILL, GROUP_CLOUD_COMPUTING, GROUP_SHIP_ADAPTION, GROUP_CHARACTER_ENHANCEMENT, dogma::MAX_SKILL_LEVEL}};
use crate::{sde::Sde, services::skills::requirements};

/// dogma/const.py attributeSkillIsObsolete.
const ATTR_SKILL_IS_OBSOLETE: i32 = 2450;

fn valid_skill_metadata(category: Option<i32>, group: Option<i32>, published: Option<bool>, obsolete: f64, cyclic: bool) -> bool {
    category == Some(CATEGORY_SKILL) && published == Some(true)
        && !group.is_some_and(|group| [GROUP_CLOUD_COMPUTING, GROUP_SHIP_ADAPTION, GROUP_CHARACTER_ENHANCEMENT].contains(&group))
        && obsolete == 0.0 && !cyclic
}
pub(crate) fn valid_skill(sde: &Sde, skill: i32) -> bool {
    sde.types.get(skill).is_some_and(|t| valid_skill_metadata(t.category_id, t.group_id, t.published,
        sde.types.attribute(skill, ATTR_SKILL_IS_OBSOLETE).unwrap_or(0.0), sde.skill_plans.is_cyclic(skill)))
}
pub(crate) fn target(sde: &Sde, type_id: i32, level: Option<i32>) -> bool {
    let Some(ty) = sde.types.get(type_id) else { return false; };
    if !target_metadata(ty.published, sde.skill_plans.is_cyclic(type_id)) { return false; }
    if let Some(level) = level {
        return (1..=MAX_SKILL_LEVEL).contains(&level) && ty.category_id == Some(CATEGORY_SKILL)
            && sde.types.attribute(type_id, ATTR_SKILL_IS_OBSOLETE).unwrap_or(0.0) == 0.0;
    }
    ty.category_id == Some(CATEGORY_SKILL) || (ty.category_id != Some(infinity_core::consts::CATEGORY_ASTEROID) && requirements::requirements(sde, type_id).next().is_some())
}

fn target_metadata(published: Option<bool>, cyclic: bool) -> bool {
    published == Some(true) && !cyclic
}

/// Client skillPlanConst.py personal plan count.
pub(crate) const MAX_PLANS: usize = 10;
/// Client skillPlanConst.py maximum steps in a plan.
pub(crate) const MAX_STEPS: usize = 150;
/// Client skillPlanConst.py maximum milestones in a plan.
pub(crate) const MAX_MILESTONES: usize = 5;
/// Client skillPlanConst.py name length.
pub(crate) const MAX_NAME: usize = 50;
/// Client skillPlanConst.py description length.
pub(crate) const MAX_DESCRIPTION: usize = 1000;

/// Python 2's narrow Unicode ABI counts UTF-16 code units, not Unicode scalar values.
pub(super) fn text_units(text: &str) -> usize { text.encode_utf16().count() }

pub(crate) fn sequence(sde: &Sde, skills: &[i32], levels: &[i32]) -> bool {
    sequence_with(skills, levels, |skill| valid_skill(sde, skill), |skill| requirements::requirements(sde, skill))
}

fn sequence_with<I: Iterator<Item = (i32, i32)>>(skills: &[i32], levels: &[i32], valid: impl Fn(i32) -> bool, direct: impl Fn(i32) -> I) -> bool {
    if skills.len() != levels.len() || skills.len() > MAX_STEPS { return false; }
    let mut previous = BTreeMap::<i32, i32>::new();
    let mut seen = HashSet::new();
    for (&skill, &level) in skills.iter().zip(levels) {
        if !valid(skill) || !(0..=MAX_SKILL_LEVEL).contains(&level) || !seen.insert((skill, level)) { return false; }
        if level == 0 { continue; }
        if previous.get(&skill).copied().unwrap_or(0) + 1 != level
            || direct(skill).any(|(id, required)| previous.get(&id).copied().unwrap_or(0) < required)
        { return false; }
        previous.insert(skill, level);
    }
    // The client checks prerequisites for every skill in the plan, including
    // marker-only entries, before checking the order of positive levels.
    for &skill in skills {
        let mut prerequisites = direct(skill).peekable();
        if prerequisites.peek().is_some() && !seen.contains(&(skill, 1)) { return false; }
        if prerequisites.any(|(id, level)| {
            !skills.contains(&id) || previous.get(&id).copied().unwrap_or(0) < level
        }) { return false; }
    }
    true
}

/// Transitive requirements, merged by maximum level; each node is expanded once.
pub(crate) fn required(sde: &Sde, type_id: i32, level: Option<i32>) -> Option<BTreeMap<i32, i32>> {
    (!sde.skill_plans.is_cyclic(type_id)).then(|| required_with(type_id, level, |id| requirements::requirements(sde, id)))
}

fn required_with<I: Iterator<Item = (i32, i32)>>(type_id: i32, level: Option<i32>, direct: impl Fn(i32) -> I) -> BTreeMap<i32, i32> {
    let mut result = BTreeMap::new();
    let mut expanded = HashSet::new();
    let mut pending = vec![type_id];
    if let Some(level) = level { result.insert(type_id, level); }
    while let Some(id) = pending.pop() {
        if !expanded.insert(id) { continue; }
        for (skill, level) in direct(id) {
            result.entry(skill).and_modify(|current: &mut i32| *current = (*current).max(level)).or_insert(level);
            pending.push(skill);
        }
    }
    result
}

/// Report legacy definitions invalid under the currently loaded static data.
pub(crate) async fn audit(db: &crate::db::Db, sde: &Sde) -> anyhow::Result<()> {
    let mut after = Vec::new();
    loop {
        let page = crate::dao::skills::plans::audit_page(db, &after).await?;
        if page.is_empty() { return Ok(()); }
        let ids: Vec<&[u8]> = page.iter().map(|(_, plan)| plan.plan_id.as_slice()).collect();
        let targets = crate::dao::skills::plan_milestones::for_plans(db, &ids).await?;
        let cyclic_plans: HashSet<Vec<u8>> = targets.into_iter()
            .filter(|target| sde.skill_plans.is_cyclic(target.type_id))
            .map(|target| target.plan_id).collect();
        for (owner, plan) in page {
            if !sequence(sde, &plan.skill_ids, &plan.levels) || cyclic_plans.contains(&plan.plan_id) {
                tracing::warn!(character_id = owner, plan_id = ?plan.plan_id, "legacy skill plan has invalid static skill sequence or cyclic milestone target");
            }
            after = plan.plan_id;
        }
    }
}

#[cfg(test)] mod tests;
