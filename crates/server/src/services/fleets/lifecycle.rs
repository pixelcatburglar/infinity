//! Final fleet departure, mirroring `eve/client/script/parklife/fleetSvc.py` lifecycle.
//! Fleet state exists only in process memory and is not persisted across restarts.

use super::*;

/// Remove a member (leaving, kicked or logged off); an empty fleet goes away and a
/// leaderless one passes the lead on.
pub(super) fn remove_member_from(st: &mut State, world: &World, fleet_id: i64, char_id: i64) {
    if !st.is_member(fleet_id, char_id) { return; }
    st.clear_member_references(char_id);
    st.of.remove(&char_id);
    let Some(f) = st.fleets.get_mut(&fleet_id) else { return };
    let Some(old) = f.members.remove(&char_id) else { return };
    world.notify_pilot(char_id, "__session", session_pairs(None, None));
    world.notify_pilot(char_id, "OnFleetLeave", vec![Value::Int(char_id)]);
    broadcast(world, f, None, "OnFleetLeave", vec![Value::Int(char_id)]);
    if f.members.is_empty() {
        st.fleets.remove(&fleet_id);
        info!("fleets: {fleet_id} disbanded");
        return;
    }
    if old.role == ROLE_LEADER {
        if let Some((&next, m)) = f.members.iter_mut().next() {
            let before = m.clone();
            m.role = ROLE_LEADER;
            m.job |= JOB_CREATOR;
            m.wing = NONE_ID;
            m.squad = NONE_ID;
            let after = m.clone();
            world.notify_pilot(next, "__session", session_pairs(Some(fleet_id), Some(&after)));
            broadcast(world, f, None, "OnFleetMemberChanged", changed(next, fleet_id, &before, &after, false));
            fleet_proxy::leader_changed(world, f, next);
        }
    }
}

/// A pilot logged off: they leave their fleet.
pub(crate) fn leave(world: &World, char_id: i64) {
    let mut st = state();
    st.clear_departing(char_id);
    if let Some(f) = st.of.get(&char_id).copied() {
        remove_member_from(&mut st, world, f, char_id);
    }
}
