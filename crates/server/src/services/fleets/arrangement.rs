//! Fleet topology validation and deterministic member placement. Fleet state is process-local;
//! arrangement uses existing wings and squads, and never creates structure.
use super::*;

pub(super) fn valid_slot(f: &Fleet, id: i64, member: &Member) -> bool {
    match member.role {
        ROLE_LEADER => member.wing == NONE_ID && member.squad == NONE_ID && !f.members.iter().any(|(ch, m)| *ch != id && m.role == ROLE_LEADER),
        ROLE_WING_COMMANDER => member.squad == NONE_ID && f.wings.iter().any(|w| w.id == member.wing) && !f.members.iter().any(|(ch, m)| *ch != id && m.role == ROLE_WING_COMMANDER && m.wing == member.wing),
        ROLE_SQUAD_COMMANDER | ROLE_MEMBER => {
            f.wings.iter().any(|w| w.id == member.wing && w.squads.iter().any(|s| s.id == member.squad))
                && (member.role != ROLE_SQUAD_COMMANDER || !f.members.iter().any(|(ch, m)| *ch != id && m.role == ROLE_SQUAD_COMMANDER && m.squad == member.squad))
                && f.members.iter().filter(|(ch, m)| **ch != id && m.squad == member.squad).count() < MAX_SQUAD_MEMBERS
        }
        _ => false,
    }
}

pub(super) fn plan(f: &Fleet, wing: i64, ids: &[i64], mode: &str, per: usize) -> Option<Vec<(i64, Member)>> {
    let squads: Vec<_> = f.wings.iter().find(|w| w.id == wing)?.squads.iter().map(|s| s.id).collect();
    if squads.is_empty() || per == 0 { return None; }
    ids.iter().enumerate().map(|(i, id)| {
        let mut member = f.members.get(id)?.clone();
        let index = match mode {
            "ArrangeMembersByAllToFirst" => 0,
            "ArrangeMembersByPreCount" => i / per,
            _ => i % squads.len(),
        };
        member.wing = wing;
        member.squad = *squads.get(index)?;
        Some((*id, member))
    }).collect()
}

pub(super) fn apply(f: &mut Fleet, changes: Vec<(i64, Member)>) -> Option<Vec<(i64, Member, Member)>> {
    let mut seen = std::collections::HashSet::new();
    if changes.iter().any(|(id, _)| !seen.insert(*id) || !f.members.contains_key(id)) { return None; }
    let old: Vec<_> = changes.iter().map(|(id, _)| (*id, f.members.remove(id).unwrap())).collect();
    let mut valid = true;
    for (id, member) in &changes {
        if !valid_slot(f, *id, member) { valid = false; break; }
        f.members.insert(*id, member.clone());
    }
    if valid && f.members.values().filter(|m| m.role == ROLE_LEADER).count() != 1 { valid = false; }
    if !valid {
        for (id, _) in &changes { f.members.remove(id); }
        for (id, member) in old { f.members.insert(id, member); }
        return None;
    }
    Some(old.into_iter().zip(changes).map(|((id, before), (_, after))| (id, before, after)).collect())
}

pub(super) fn publish(world: &World, fleet_id: i64, f: &Fleet, changes: Vec<(i64, Member, Member)>) {
    let only = f.members.len() == 1;
    for (id, before, after) in changes {
        if after.reconnect.is_none() { world.notify_pilot(id, "__session", session_pairs(Some(fleet_id), Some(&after))); }
        broadcast(world, f, None, "OnFleetMemberChanged", changed(id, fleet_id, &before, &after, only));
    }
}

#[cfg(test)]
#[path = "tests/arrangement.rs"]
mod tests;
