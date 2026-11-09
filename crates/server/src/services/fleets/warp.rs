//! Fleet warp mirrors the client CmdWarpToStuff fleet flag, including mission and scan routes.
//! Authority and consent are process-local and rechecked at actor commit using live fleet
//! membership, space actors and fitted ship projections. Each accepted member pays
//! independently at activation; one callback completes on the first full arrival or
//! immediate native approach, not a partial trip. Persisted fleets, bookmark
//! destination resolution and fleet boosts are not supported.
use super::*;
use infinity_core::messages;
use crate::{client::Client, space::{self, system::{Timer, warp::WarpDestination}}};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Position { wing: i64, squad: i64, role: i64 }
impl From<&Member> for Position {
    fn from(m: &Member) -> Self { Self { wing: m.wing, squad: m.squad, role: m.role } }
}
#[derive(Clone)]
pub(crate) struct Order { fleet: i64, commander: i64, ship: i64, position: Position, members: Vec<(i64, Position)>, target: Option<(i64, i64)> }
pub(super) fn can_command(member: &Member) -> bool {
    matches!(member.role, ROLE_LEADER | ROLE_WING_COMMANDER | ROLE_SQUAD_COMMANDER)
}

pub(super) fn in_scope(commander: &Member, member: &Member) -> bool {
    match commander.role {
        ROLE_LEADER => true,
        ROLE_WING_COMMANDER => commander.wing != NONE_ID && member.wing == commander.wing,
        ROLE_SQUAD_COMMANDER => commander.squad != NONE_ID && member.wing == commander.wing && member.squad == commander.squad,
        _ => false,
    }
}
fn snapshot(st: &State, commander: i64, ship: i64) -> Option<Order> {
    let fleet = *st.of.get(&commander)?;
    if !st.is_member(fleet, commander) { return None; }
    let f = st.fleets.get(&fleet)?;
    let commander_member = f.members.get(&commander)?;
    let position = Position::from(commander_member);
    if !can_command(commander_member) { return None; }
    let mut members: Vec<_> = f.members.iter().filter_map(|(&id, m)| {
        let p = Position::from(m);
        (st.is_member(fleet, id) && m.opt_outs.accepts_fleet_warp && in_scope(commander_member, m)).then_some((id, p))
    }).collect();
    members.sort_by_key(|(id, _)| (*id == commander, *id));
    Some(Order { fleet, commander, ship, position, members, target: None })
}
pub(in crate::services) async fn authorize(c: &Client) -> Result<Order, CallError> {
    let commander = c.session.int("charid").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    let order = snapshot(&state(), commander, ship).ok_or_else(|| notify_error(messages::fleets::WARP_NOT_AUTHORIZED))?;
    let Some(system) = c.session.int("solarsystemid") else { return Err(notify_error(messages::fleets::WARP_NOT_AUTHORIZED)) };
    if !c.world.with(system, move |s| live(s, commander, ship)).await.unwrap_or(false) {
        return Err(notify_error(messages::fleets::WARP_NOT_AUTHORIZED));
    }
    Ok(order)
}
pub(in crate::services) fn permits_target(order: &Order, pilot: i64) -> bool {
    let st = state();
    st.is_member(order.fleet, order.commander) && st.is_member(order.fleet, pilot)
}
pub(in crate::services) fn target(order: &mut Order, pilot: i64, ball: i64) {
    order.target = Some((pilot, ball));
}
fn current(st: &State, order: &Order, pilot: i64, position: Position) -> bool {
    if !st.is_member(order.fleet, order.commander) || !st.is_member(order.fleet, pilot) { return false; }
    let f = &st.fleets[&order.fleet];
    let Some(commander) = f.members.get(&order.commander) else { return false };
    let Some(member) = f.members.get(&pilot) else { return false };
    Position::from(commander) == order.position && Position::from(member) == position
        && member.opt_outs.accepts_fleet_warp && in_scope(commander, member)
}
fn live(s: &space::System, pilot: i64, ship: i64) -> bool {
    s.pilot_ball(pilot).is_some_and(|b| b.id == ship && b.char_id == Some(pilot) && b.owner_id == pilot && b.cloak == 0 && b.mode != space::MODE_WARP)
}
/// The lock covers both the final consent check and enqueue, with no await or World access.
pub(in crate::services) fn commit<T>(s: &mut space::System, order: &Order, pilot: i64, ship: i64, f: impl FnOnce(&mut space::System) -> T) -> Option<T> {
    let st = state();
    commit_in(&st, s, order, pilot, ship, f)
}
fn commit_in<T>(st: &State, s: &mut space::System, order: &Order, pilot: i64, ship: i64, f: impl FnOnce(&mut space::System) -> T) -> Option<T> {
    let position = order.members.iter().find(|(id, _)| *id == pilot)?.1;
    if !current(st, order, pilot, position) || !live(s, order.commander, order.ship) || !live(s, pilot, ship) { return None; }
    if let Some((target, ball)) = order.target {
        if !st.is_member(order.fleet, target)
            || !s.pilot_ball(target).is_some_and(|b| b.id == ball && b.char_id == Some(target) && b.owner_id == target)
            || !s.visible_to(order.commander, ball) { return None; }
    }
    Some(f(s))
}

pub(in crate::services) async fn execute(db: &crate::db::Db, world: &space::World, space_system: Option<i64>, order: Order, destination: WarpDestination, on_arrival: Option<Timer>, fleet_destination: Value) -> CallResult {
    if !destination.is_valid() { return Ok(Value::None); }
    let Some(system) = space_system else { return Ok(Value::None) };
    let ids: Vec<_> = order.members.iter().map(|(id, _)| *id).collect();
    let commander = order.commander;
    let ship = order.ship;
    let Some(ships) = world.with(system, move |s| {
        if !live(s, commander, ship) { return Vec::new(); }
        ids.into_iter().filter_map(|id| s.pilot_ball(id).filter(|b| live(s, id, b.id)).map(|b| (id, b.id))).collect::<Vec<_>>()
    }).await else { return Ok(Value::None) };
    let guard = match on_arrival {
        Some(callback) => match world.begin_warp_arrival(system, callback).await {
            Some(guard) => Some(guard), None => return Ok(Value::None),
        },
        None => None,
    };
    let order = std::sync::Arc::new(order);
    for (pilot, ship) in ships {
        let queued = super::super::ship::warp_pilot(db, world, system, pilot, ship, destination, guard.as_ref().map(|g| g.id()), Some(order.clone())).await?;
        if queued { world.notify_pilot(pilot, "OnRemoteMessage", vec![Value::str("FleetWarp"), fleet_destination.clone()]); }
    }
    Ok(Value::None)
}

#[cfg(test)]
#[path = "tests/warp.rs"]
mod tests;
