//! Fleet broadcast filtering for evefleet/util.py and fleetSvc.py.
//! Fleet topology is process-local; targets come from the live system actor or SDE.
//! A system has one ballpark, not separate distance-based grids. Active cynosural
//! beacon broadcasts are unavailable because the server does not model them.

use super::*;

/// Destination is independent of the client hierarchy scope.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BroadcastDestination { Universe, System, Bubble }

/// evefleet/const.py broadcast hierarchy scopes.
const DOWN: i64 = 1;
const UP: i64 = 2;
const ALL: i64 = 3;
/// inventorycommon/const.py categoryFighter.
const CATEGORY_FIGHTER: i32 = 87;

pub(super) fn allowed(scope: i64, sender_id: i64, sender: &Member, recipient_id: i64, recipient: &Member) -> bool {
    if scope == ALL || sender_id == recipient_id { return true; }
    match scope {
        UP => sender.role != ROLE_LEADER && (recipient.role == ROLE_LEADER ||
            (sender.squad > 0 && recipient.role != ROLE_MEMBER && recipient.wing == sender.wing)),
        DOWN => sender.role == ROLE_LEADER || (sender.squad > 0 && recipient.squad == sender.squad) ||
            (sender.role == ROLE_WING_COMMANDER && recipient.wing == sender.wing),
        _ => false,
    }
}

fn known(name: &str) -> bool {
    matches!(name, "EnemySpotted" | "NeedBackup" | "HoldPosition" | "InPosition" | "TravelTo" |
        "JumpBeacon" | "Location" | "Target" | "HealTarget" | "HealArmor" | "HealShield" |
        "HealCapacitor" | "WarpTo" | "AlignTo" | "JumpTo")
}


fn route(name: &str) -> BroadcastDestination {
    match name {
        "Target" | "HealTarget" | "HealArmor" | "HealShield" | "HealCapacitor" => BroadcastDestination::Bubble,
        "WarpTo" | "AlignTo" | "JumpTo" => BroadcastDestination::System,
        _ => BroadcastDestination::Universe,
    }
}
fn optional_int(value: Option<&Value>) -> Option<Option<i64>> {
    match value { None | Some(Value::None) => Some(None), Some(v) => v.as_int().map(Some) }
}

pub(super) async fn send(c: &mut Client, bound: Option<i64>, call: &Call<'_>, destination: BroadcastDestination) -> CallResult {
    let invalid = || notify_error(messages::fleets::INVALID_REQUEST);
    let name = call.args.first().and_then(Value::as_text).ok_or_else(invalid)?.into_owned();
    let scope = call.args.get(1).and_then(Value::as_int).ok_or_else(invalid)?;
    let item = optional_int(call.args.get(2)).ok_or_else(invalid)?;
    let supplied_type = optional_int(call.args.get(3)).ok_or_else(invalid)?;
    if !known(&name) || !matches!(scope, DOWN | UP | ALL) || route(&name) != destination || call.args.len() > 4 { return Err(invalid()); }
    // Active cynosural beacons have no server-side owner in the current space model.
    if name == "JumpBeacon" { return Err(invalid()); }
    let me = c.session.int("charid").ok_or_else(invalid)?;
    let system = c.session.int("solarsystemid").ok_or_else(invalid)?;
    let ship = c.session.int("shipid").ok_or_else(invalid)?;
    let (fleet_id, candidates) = {
        let st = state();
        let fleet_id = *st.of.get(&me).ok_or_else(invalid)?;
        if bound.is_some_and(|id| id != fleet_id) || !st.is_member(fleet_id, me) { return Err(invalid()); }
        let fleet = &st.fleets[&fleet_id];
        let sender = &fleet.members[&me];
        let candidates = fleet.members.iter().filter_map(|(&id, member)|
            (st.is_member(fleet_id, id) && allowed(scope, me, sender, id, member)).then_some(id)).collect::<Vec<_>>();
        (fleet_id, candidates)
    };
    let travel = name == "TravelTo";
    if travel && (item.is_none_or(|id| c.sde.map.system(id).is_none()) || supplied_type.is_some()) { return Err(invalid()); }
    let optional_target = matches!(name.as_str(), "EnemySpotted" | "NeedBackup" | "HoldPosition" | "InPosition" | "Location");
    if !travel && item.is_none() && !optional_target { return Err(invalid()); }
    if item.is_none() && supplied_type.is_some() { return Err(invalid()); }
    let actor_name = name.clone();
    let result = c.world.with(system, move |s| {
        let source = s.pilot_ball(me)?;
        if source.id != ship { return None; }
        let cloak = source.cloak;
        let target = if travel { None } else if let Some(id) = item {
            let ball = s.ball(id)?;
            if !s.fleet_broadcast_visible(me, id) { return None; }
            if matches!(actor_name.as_str(), "HealArmor" | "HealShield" | "HealCapacitor") && id != ship { return None; }
            Some((i64::from(ball.type_id), ball.id))
        } else { None };
        let recipients = candidates.into_iter().filter(|&id| {
            if cloak != 0 && id != me { return false; }
            match destination {
                BroadcastDestination::Universe => true,
                BroadcastDestination::System => s.pilot_ball(id).is_some(),
                BroadcastDestination::Bubble => s.fleet_broadcast_visible(id, ship) &&
                    target.is_none_or(|(_, target_id)| s.fleet_broadcast_visible(id, target_id)),
            }
        }).collect::<Vec<_>>();
        Some((target.map(|(kind, _)| kind), recipients))
    }).await.flatten().ok_or_else(invalid)?;
    let (actual_type, recipients) = result;
    if supplied_type.is_some() && supplied_type != actual_type { return Err(invalid()); }
    if matches!(name.as_str(), "WarpTo" | "AlignTo" | "JumpTo") {
        let kind = actual_type.and_then(|id| i32::try_from(id).ok()).and_then(|id| c.sde.types.get(id)).ok_or_else(invalid)?;
        if name == "JumpTo" {
            if item.is_none_or(|id| c.sde.map.stargate(id).is_none()) { return Err(invalid()); }
        } else if matches!(kind.category_id, Some(infinity_core::consts::CATEGORY_ENTITY | infinity_core::consts::CATEGORY_DRONE | infinity_core::consts::CATEGORY_SHIP | CATEGORY_FIGHTER)) {
            return Err(invalid());
        }
        if supplied_type.is_none() { return Err(invalid()); }
    }
    let args = vec![Value::str(&name), Value::Int(scope), Value::Int(me), Value::Int(system), item.map(Value::Int).unwrap_or(Value::None), supplied_type.map(Value::Int).unwrap_or(Value::None)];
    let st = state();
    if !st.is_member(fleet_id, me) { return Err(invalid()); }
    let fleet = &st.fleets[&fleet_id];
    let sender = &fleet.members[&me];
    for id in recipients {
        if st.is_member(fleet_id, id) && allowed(scope, me, sender, id, &fleet.members[&id]) {
            c.world.notify_pilot(id, "OnFleetBroadcast", args.clone());
        }
    }
    Ok(Value::None)
}
