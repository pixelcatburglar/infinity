//! Read-only chronological skill, booster, and expert-system projection for characterskills/util.py.
//! The plan captures initial installations so historical fits retain grants through expiry.
//! Completions precede booster and expert expiries at equal timestamps.
//! Historical Fit snapshots cannot reconstruct brain items already removed from the database.
use std::{collections::HashMap, sync::Arc};
use infinity_core::consts::FLAG_BOOSTER;
use crate::{dao::{items::brain::BrainItem, skills::{expert_systems::ExpertSystem, queue::QueueEntry, training::Skill}}, db::executor::Executor, sde::Sde, services::{db_err, fit::{Fit, Key}, CallError}};
use super::training;
use infinity_core::consts::dogma::CHAR_ATTRIBUTES;

pub(super) struct Completion { pub type_id: i32, pub level: i32, pub points: i64, pub end_time: i64 }
pub(super) enum Event { Completion(Completion), Expiry { at: i64, items: Vec<BrainItem> }, ExpertExpiry { installations: Vec<ExpertSystem> } }

/// Final persisted state excludes partial progress at the cutoff unless an
/// earlier attribute change required banking it. `trained_points` includes it.
pub(super) struct AdvancePlan {
    pub char_id: i64,
    pub through: i64,
    pub initial_skills: Vec<Skill>,
    pub initial_queue: Vec<QueueEntry>,
    pub initial_brain: Vec<BrainItem>,
    pub initial_installations: Vec<ExpertSystem>,
    pub skills: Vec<Skill>,
    pub queue: Vec<QueueEntry>,
    pub events: Vec<Event>,
    pub queue_changed: bool,
    pub brain_changed: bool,
    pub boosters_changed: bool,
}


impl AdvancePlan {
    pub(super) fn trained_points(&self) -> Result<i64, CallError> {
        let mut total = 0i64;
        for skill in &self.skills {
            let mut points = skill.skill_points;
            if let Some(row) = self.queue.first().filter(|row| row.type_id == skill.type_id) {
                if let (Some(start), Some(end)) = (row.start_time, row.end_time) {
                    points = points.max(training::points_at(row.start_sp, row.dest_sp, start, end, self.through));
                }
            }
            total = total.checked_add(points).ok_or_else(|| CallError::Internal(anyhow::anyhow!("skill points overflow")))?;
        }
        Ok(total)
    }
}

/// Caller must serialize the character and provide a consistent transaction.
/// This function only reads; applying the result requires guarding observed state.
pub(super) async fn plan_tx(ex: &(impl Executor + ?Sized), sde: &Arc<Sde>, char_id: i64, through: i64) -> Result<AdvancePlan, CallError> {
    let mut initial_skills = crate::dao::skills::training::current(ex, char_id).await.map_err(db_err)?;
    initial_skills.sort_unstable_by_key(|skill| skill.type_id);
    let initial_queue = crate::dao::skills::queue::list(ex, char_id).await.map_err(db_err)?;
    let initial_brain = crate::dao::items::brain::list(ex, char_id).await.map_err(db_err)?;
    let initial_installations = crate::dao::skills::expert_systems::all(ex, char_id).await.map_err(db_err)?;
    let mut plan = AdvancePlan { char_id, skills: initial_skills.clone(), queue: initial_queue.clone(), initial_skills, initial_queue, initial_brain, initial_installations, through, events: Vec::new(), queue_changed: false, brain_changed: false, boosters_changed: false };
    let mut expired = Vec::new();
    let mut expert_expired = Vec::new();
    loop {
        let completion = plan.queue.first().and_then(|row| row.end_time).filter(|&at| at <= through);
        let expiry = plan.initial_brain.iter().filter(|row| row.flag_id == FLAG_BOOSTER && !expired.contains(&row.item_id)).filter_map(|row| row.expires_at).filter(|&at| at <= through).min();
        let expert_expiry = plan.initial_installations.iter().filter(|row| row.installed_at <= row.expires_at && row.expires_at <= through && !expert_expired.contains(&row.type_id)).map(|row| row.expires_at).min();
        let next_expiry = expiry.into_iter().chain(expert_expiry).min();
        if let Some(at) = completion.filter(|at| next_expiry.is_none_or(|expiry| *at <= expiry)) {
            let row = plan.queue.remove(0);
            for (position, entry) in plan.queue.iter_mut().enumerate() { entry.position = position as i32; }
            let skill = if let Some(index) = plan.skills.iter().position(|s| s.type_id == row.type_id) { &mut plan.skills[index] } else {
                plan.skills.push(Skill { type_id: row.type_id, level: 0, skill_points: 0 });
                plan.skills.last_mut().unwrap()
            };
            if row.to_level > skill.level {
                plan.events.push(Event::Completion(Completion { type_id: row.type_id, level: row.to_level, points: row.dest_sp, end_time: at }));
                skill.level = row.to_level;
            }
            skill.skill_points = skill.skill_points.max(row.dest_sp);
            plan.queue_changed = true;
            continue;
        }
        let Some(at) = next_expiry else { break };
        let before_at = at.checked_sub(1).ok_or_else(|| CallError::Internal(anyhow::anyhow!("skill expiry time underflow")))?;
        let before = Fit::load_character_with_skills_at(ex, Arc::clone(sde), char_id, before_at, &plan.skills, &plan.initial_installations).await?;
        let after = Fit::load_character_with_skills_at(ex, Arc::clone(sde), char_id, at, &plan.skills, &plan.initial_installations).await?;
        let old: HashMap<_, _> = CHAR_ATTRIBUTES.iter().map(|&id| (id, before.value(Key::Char, id as i32))).collect();
        let new: HashMap<_, _> = CHAR_ATTRIBUTES.iter().map(|&id| (id, after.value(Key::Char, id as i32))).collect();
        let items: Vec<_> = plan.initial_brain.iter().filter(|row| row.flag_id == FLAG_BOOSTER && row.expires_at == Some(at)).cloned().collect();
        expired.extend(items.iter().map(|row| row.item_id));
        if !items.is_empty() {
            plan.events.push(Event::Expiry { at, items });
            plan.brain_changed = true;
            plan.boosters_changed = true;
        }
        let installations: Vec<_> = plan.initial_installations.iter().filter(|row| row.expires_at == at && !expert_expired.contains(&row.type_id)).cloned().collect();
        expert_expired.extend(installations.iter().map(|row| row.type_id));
        if !installations.is_empty() {
            plan.events.push(Event::ExpertExpiry { installations });
            plan.brain_changed = true;
        }
        if old != new {
            if let Some(row) = plan.queue.first_mut() {
                if let (Some(start), Some(end)) = (row.start_time, row.end_time) {
                    let interpolated = training::points_at(row.start_sp, row.dest_sp, start, end, at);
                    let banked = if let Some(skill) = plan.skills.iter_mut().find(|skill| skill.type_id == row.type_id) {
                        skill.skill_points = skill.skill_points.max(interpolated);
                        skill.skill_points.min(row.dest_sp)
                    } else { interpolated };
                    row.start_sp = banked;
                    let mut time = at;
                    for row in &mut plan.queue { time = training::schedule(sde, row, &new, time)?; }
                    plan.queue_changed = true;
                }
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests;
