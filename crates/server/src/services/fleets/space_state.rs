//! Fleet space state for `eve/client/script/parklife/fleetSvc.py`.
//! Membership and tags are process-local; positions and visibility come from system actors.
//! Active fleet leaders, wing commanders and squad commanders can set or clear tags.
//! Offline, out-of-system and invisible pilots are omitted.
use super::*;

/// The client's tag menu offers digits 0-9 and these letters (menusvc.py:2057-2058).
const TAG_LETTERS: &[u8] = b"ABCDEFGHIJXYZ";

pub(crate) async fn locations(c: &Client) -> CallResult {
    let Some(me) = c.session.int("charid") else { return Ok(Value::Dict(vec![])) };
    let Some(system) = c.session.int("solarsystemid") else { return Ok(Value::Dict(vec![])) };
    let Some(ship) = c.session.int("shipid") else { return Ok(Value::Dict(vec![])) };
    let (fleet_id, members) = {
        let st = state();
        let Some(&fleet_id) = st.of.get(&me) else { return Ok(Value::Dict(vec![])) };
        if !st.is_member(fleet_id, me) { return Ok(Value::Dict(vec![])); }
        let members = st.fleets[&fleet_id].members.iter().filter_map(|(&id, member)| member.reconnect.is_none().then_some(id)).collect::<std::collections::HashSet<_>>();
        (fleet_id, members)
    };
    let found = c.world.with(system, move |s| {
        if s.pilot_ball(me).is_none_or(|ball| ball.id != ship) || !s.fleet_broadcast_visible(me, ship) { return Vec::new(); }
        s.pilots().filter_map(|ball| {
            let id = ball.char_id?;
            if !members.contains(&id) || !s.fleet_broadcast_visible(me, ball.id) { return None; }
            let pos = s.sample_position(ball.id)?;
            pos.iter().all(|v| v.is_finite()).then_some((id, pos))
        }).collect::<Vec<_>>()
    }).await.unwrap_or_default();
    let st = state();
    if !st.is_member(fleet_id, me) { return Ok(Value::Dict(vec![])); }
    Ok(Value::Dict(found.into_iter().filter(|(id, _)| st.is_member(fleet_id, *id) && c.world.is_online(*id)).map(|(id, pos)| {
        (Value::Int(id), keyval([("pos", Value::tuple(pos.into_iter().map(Value::Float)))]))
    }).collect()))
}

fn snapshot(fleet: Option<&Fleet>, system: i64, actor: &crate::space::System, character: i64, clears: &[i64]) -> Value {
    let mut tags = fleet.into_iter().flat_map(|f| f.target_tags.iter()).filter_map(|(&(sys, item), tag)| {
        (sys == system && !clears.contains(&item) && actor.fleet_broadcast_visible(character, item)).then(|| (Value::Int(item), Value::str(tag)))
    }).collect::<Vec<_>>();
    tags.extend(clears.iter().map(|&item| (Value::Int(item), Value::None)));
    keyval([("targetTags", Value::Dict(tags))])
}

pub(crate) async fn get(c: &Client) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let Some(system) = c.session.int("solarsystemid") else { return Ok(keyval([("targetTags", Value::Dict(vec![]))])) };
    let ship = c.session.int("shipid").unwrap_or(0);
    Ok(c.world.with(system, move |actor| {
        let st = state();
        let fleet = st.of.get(&me).filter(|&&id| st.is_member(id, me)).and_then(|id| st.fleets.get(id));
        if actor.pilot_ball(me).is_none_or(|b| b.id != ship) { return snapshot(None, system, actor, me, &[]); }
        snapshot(fleet, system, actor, me, &[])
    }).await.unwrap_or_else(|| keyval([("targetTags", Value::Dict(vec![]))])))
}

pub(crate) async fn tag(c: &Client, call: &Call<'_>) -> CallResult {
    let invalid = || notify_error(messages::fleets::INVALID_REQUEST);
    if call.args.len() != 2 { return Err(invalid()); }
    let item = call.args[0].as_int().filter(|id| *id > 0).ok_or_else(invalid)?;
    let tag = match &call.args[1] {
        Value::None => None,
        v => {
            let text = v.as_text().ok_or_else(invalid)?;
            if text.len() != 1 || !text.bytes().all(|b| b.is_ascii_digit() || TAG_LETTERS.contains(&b)) { return Err(invalid()); }
            Some(text.into_owned())
        }
    };
    let me = c.session.int("charid").ok_or_else(invalid)?;
    let system = c.session.int("solarsystemid").ok_or_else(invalid)?;
    let ship = c.session.int("shipid").ok_or_else(invalid)?;
    let sde = c.sde.clone();
    let accepted = c.world.with(system, move |actor| {
        let Some(target) = actor.ball(item) else { return false };
        if item == ship || actor.pilot_ball(me).is_none_or(|b| b.id != ship) || !actor.fleet_broadcast_visible(me, item) { return false; }
        let Some(group) = sde.types.get(target.type_id).and_then(|t| t.group_id) else { return false };
        if matches!(group, infinity_core::consts::GROUP_PLANET | infinity_core::consts::GROUP_ASTEROID_BELT | infinity_core::consts::GROUP_MOON) { return false; }
        let mut st = state();
        let Some(&fleet_id) = st.of.get(&me) else { return false };
        if !st.is_member(fleet_id, me) || !st.fleets[&fleet_id].members.get(&me).is_some_and(|m| matches!(m.role, ROLE_LEADER | ROLE_WING_COMMANDER | ROLE_SQUAD_COMMANDER)) { return false; }
        let fleet = st.fleets.get_mut(&fleet_id).unwrap();
        let clears = if let Some(tag) = tag { fleet.target_tags.insert((system, item), tag); Vec::new() } else { fleet.target_tags.remove(&(system, item)); vec![item] };
        for (&recipient, member) in &fleet.members {
            if member.reconnect.is_none() && actor.fleet_broadcast_visible(recipient, item) {
                actor.notify_pilot(recipient, "OnFleetStateChange", vec![snapshot(Some(fleet), system, actor, recipient, &clears)]);
            }
        }
        true
    }).await.unwrap_or(false);
    if accepted { Ok(Value::None) } else { Err(invalid()) }
}

/// Clear removed targets and the departing pilot's local labels before removing the ball.
pub(crate) fn ball_removed(actor: &crate::space::System, system: i64, item: i64, departing: Option<i64>) {
    let mut st = state();
    for fleet in st.fleets.values_mut() {
        let removed = fleet.target_tags.remove(&(system, item)).is_some();
        for (&recipient, member) in &fleet.members {
            if member.reconnect.is_some() || actor.pilot_ball(recipient).is_none() { continue; }
            let clears = if Some(recipient) == departing {
                fleet.target_tags.keys().filter_map(|&(sys, id)| (sys == system).then_some(id)).chain(removed.then_some(item)).collect::<Vec<_>>()
            } else if removed && actor.fleet_broadcast_visible(recipient, item) { vec![item] } else { continue };
            if clears.is_empty() { continue; }
            actor.notify_pilot(recipient, "OnFleetStateChange", vec![snapshot(Some(fleet), system, actor, recipient, &clears)]);
        }
    }
}

/// Synchronize a newly established ballpark observer with its current fleet.
pub(crate) fn observe(actor: &crate::space::System, system: i64, character: i64) {
    let st = state();
    let fleet = st.of.get(&character).filter(|&&id| st.is_member(id, character)).and_then(|id| st.fleets.get(id));
    actor.notify_pilot(character, "OnFleetStateChange", vec![snapshot(fleet, system, actor, character, &[])]);
}

/// Clear the departing member's local overview without disclosing other systems.
pub(super) fn leave(world: &World, fleet: &Fleet, character: i64) {
    let Some(system) = world.online_session(character).and_then(|s| s.system_id) else { return };
    let clears = fleet.target_tags.keys().filter_map(|&(sys, id)| (sys == system).then_some(id)).collect::<Vec<_>>();
    world.post(system, move |actor| {
        if actor.pilot_ball(character).is_some() {
            let st = state();
            let current = st.of.get(&character).filter(|&&id| st.is_member(id, character)).and_then(|id| st.fleets.get(id));
            let clears = clears.into_iter().filter(|item| current.is_none_or(|fleet| !fleet.target_tags.contains_key(&(system, *item)))).collect::<Vec<_>>();
            actor.notify_pilot(character, "OnFleetStateChange", vec![snapshot(current, system, actor, character, &clears)]);
        }
    });
}

#[cfg(test)]
mod tests;
