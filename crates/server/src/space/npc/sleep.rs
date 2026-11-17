//! Spawn-origin marking and NPC-private readiness for in-process sleep.
//! A group tag names the content authority that produced the cohort; the same
//! structural proof serves the live unload admission and the cold restore, so
//! the two sides can never disagree about what a safe cohort is. No NPC AI
//! lives here: brains, balls and combat records stay owned by `System`.

use super::*;
use crate::space::system::npc_sleep::{NpcSpawnOrigin, SleepingNpcs};

/// Every group must carry a sleepable origin, stay non-empty and hold only
/// NPCs whose reverse membership is exactly that one group. `npcs` is the
/// cohort's own brain set: live `System` brains on admission, or the stored
/// payload on cold restore.
fn groups_sleepable(groups: &HashMap<u64, SpawnGroup>, member_groups: &HashMap<i64, HashSet<u64>>,
    npcs: &HashMap<i64, NpcBrain>) -> Option<&'static str> {
    for (&id, group) in groups {
        if group.origin.is_none() { return Some("npc_group_origin"); }
        if group.members.is_empty() { return Some("npc_group_empty"); }
        for member in &group.members {
            if !npcs.contains_key(member) { return Some("npc_group_member"); }
            match member_groups.get(member) {
                Some(set) if set.len() == 1 && set.contains(&id) => {}
                _ => return Some("npc_group_membership"),
            }
        }
    }
    for (member, set) in member_groups {
        if set.len() != 1 || !npcs.contains_key(member)
            || set.iter().next().is_none_or(|id| groups.get(id).is_none_or(|g| !g.members.contains(member))) {
            return Some("npc_group_membership");
        }
    }
    if npcs.keys().any(|id| !member_groups.contains_key(id)) { return Some("npc_group_membership"); }
    None
}

impl System {
    /// Tag one live, non-empty, all-NPC spawn group with its spawn origin so a
    /// proven-quiescent unload may preserve it in the world's sleep store.
    /// Refuses unknown, cleared, mixed or differently-tagged groups and never
    /// runs the group's clear callback.
    pub fn mark_npc_group_sleepable(&mut self, group: u64, origin: NpcSpawnOrigin) -> bool {
        let Some(entry) = self.groups.get(&group) else { return false };
        if entry.origin.is_some_and(|o| o != origin)
            || entry.members.is_empty()
            || !entry.members.iter().all(|&m| self.contains(m) && self.brains.contains_key(&m)) {
            return false;
        }
        self.groups.get_mut(&group).expect("checked group").origin = Some(origin);
        true
    }

    /// NPC-private readiness shared by the live admission proof and the
    /// supervisor's payload move: every group marked and membership symmetric,
    /// every member a live NPC ball, and every brain target-free, unannounced
    /// and without a positive local repair cycle (nonfinite input refuses
    /// too). A brain still holding a bound turret ramp or a live beam is not
    /// quiescent: it must never carry a dead generation's beam into sleep.
    pub(in crate::space) fn npc_sleep_internals_blocked(&self) -> Option<&'static str> {
        if let Some(reason) = groups_sleepable(&self.groups, &self.member_groups, &self.brains) {
            return Some(reason);
        }
        for (&id, brain) in &self.brains {
            if self.ball(id).is_none() { return Some("npc_ball"); }
            if brain.target.is_some() || brain.announced { return Some("npc_target"); }
            if brain.turret_spool.binding().is_some() || brain.beam.is_some() { return Some("npc_beam"); }
            let (shield, armor, _) = brain.config.repairs;
            if !shield.is_finite() || !armor.is_finite() || shield > 0.0 || armor > 0.0 {
                return Some("npc_repair");
            }
        }
        None
    }

    /// The same group/membership symmetry against a stored payload's own sets.
    /// Called by the cold-restore borrow check before the payload is consumed.
    pub(in crate::space) fn sleeping_npcs_groups_blocked(payload: &SleepingNpcs) -> Option<&'static str> {
        groups_sleepable(&payload.groups, &payload.member_groups, &payload.brains)
    }
}
