//! Fleet departure and reconnect reservations, mirroring fleetSvc.py lifecycle.
//! Fleet state is process-local and does not survive restart.

use super::*;
use std::time::Duration;

/// Remove a structural seat; reserved pilots must never receive direct notifications.
pub(super) fn remove_member_from(st: &mut State, world: &World, fleet_id: i64, char_id: i64) {
    if !st.has_member(fleet_id, char_id) { return; }
    st.clear_member_references(char_id);
    st.of.remove(&char_id);
    let Some(f) = st.fleets.get_mut(&fleet_id) else { return };
    space_state::leave(world, f, char_id);
    let Some(old) = f.members.remove(&char_id) else { return };
    if old.reconnect.is_none() {
        world.notify_pilot(char_id, "__session", session_pairs(None, None));
        world.notify_pilot(char_id, "OnFleetLeave", vec![Value::Int(char_id)]);
    }
    broadcast(world, f, None, "OnFleetLeave", vec![Value::Int(char_id)]);
    if f.members.is_empty() {
        st.fleets.remove(&fleet_id);
        info!("fleets: {fleet_id} disbanded");
        return;
    }
    if old.role == ROLE_LEADER {
        let next = f.members.iter().find(|(_, m)| m.reconnect.is_none()).or_else(|| f.members.iter().next()).map(|(&id, _)| id);
        if let Some(next) = next {
            let m = f.members.get_mut(&next).unwrap();
            let before = m.clone();
            m.role = ROLE_LEADER;
            m.job |= JOB_CREATOR;
            m.wing = NONE_ID;
            m.squad = NONE_ID;
            let after = m.clone();
            if after.reconnect.is_none() { world.notify_pilot(next, "__session", session_pairs(Some(fleet_id), Some(&after))); }
            broadcast(world, f, None, "OnFleetMemberChanged", changed(next, fleet_id, &before, &after, false));
            fleet_proxy::leader_changed(world, f, next);
        }
    }
}

/// Reserve only the session that owns an active seat; repeated cleanup never renews it.
pub(super) fn reserve(st: &mut State, char_id: i64, sid: i64, now: Instant, timeout: Duration) -> Option<(i64, u64, Instant)> {
    let Some(fleet_id) = st.of.get(&char_id).copied() else {
        st.clear_disconnected_pending(char_id, sid);
        return None;
    };
    let member = st.fleets.get_mut(&fleet_id)?.members.get_mut(&char_id)?;
    if let Some(reservation) = &mut member.reconnect {
        if reservation.offered_session.is_some_and(|owner| owner != sid) { return None; }
        reservation.offered_session = None;
        // With no offer, live_sessions has already serialized same-account replacement.
        st.clear_disconnected_pending(char_id, sid);
        return None;
    }
    if member.active_session != Some(sid) { return None; }
    st.generation = st.generation.checked_add(1).expect("fleet reservation generation exhausted");
    let deadline = now + timeout;
    member.reconnect = Some(ReconnectState { generation: st.generation, deadline, offered_session: None });
    member.active_session = None;
    member.recovered_generation = None;
    member.damage_updates = false;
    let generation = st.generation;
    st.clear_disconnected_pending(char_id, sid);
    Some((fleet_id, generation, deadline))
}

pub(super) fn expire(st: &mut State, world: &World, fleet_id: i64, char_id: i64, generation: u64, deadline: Instant, now: Instant) {
    let matches = st.has_member(fleet_id, char_id) && st.fleets[&fleet_id].members[&char_id].reconnect.as_ref().is_some_and(|r| r.generation == generation && r.deadline == deadline && now >= deadline);
    if matches { remove_member_from(st, world, fleet_id, char_id); }
}

pub(crate) fn disconnect(world: &std::sync::Arc<World>, char_id: i64, sid: i64) {
    let reservation = reserve(&mut state(), char_id, sid, Instant::now(), Duration::from_secs(crate::config::game().fleets.reconnect_timeout_secs));
    if let Some((fleet_id, generation, deadline)) = reservation {
        let world = world.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            expire(&mut state(), &world, fleet_id, char_id, generation, deadline, Instant::now());
        });
    }
}

/// Explicit final departure preserves unrelated incoming offers.
pub(crate) fn leave(world: &World, char_id: i64) {
    let mut st = state();
    if let Some(f) = st.of.get(&char_id).copied() {
        remove_member_from(&mut st, world, f, char_id);
    } else {
        st.fleets.retain(|_, f| f.initialized || f.creator != char_id);
    }
}
