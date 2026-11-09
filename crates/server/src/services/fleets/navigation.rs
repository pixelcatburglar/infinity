//! Fleet regroup uses the command and preference exposed by `eve/client/script/parklife/fleetSvc.py`.
//! Fleet membership and consent are process-local; positions come from the system actor.
//! The client does not specify the server's regroup algorithm: moving eligible ships
//! toward the commander's sampled position without warping is a server approximation.
//! Cross-system regroup and persistent fleet state are not supported.

use super::*;
use crate::space::{self, event};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Position { wing: i64, squad: i64, role: i64 }
impl From<&Member> for Position {
    fn from(m: &Member) -> Self { Self { wing: m.wing, squad: m.squad, role: m.role } }
}

struct Order {
    fleet: i64,
    commander: i64,
    ship: i64,
    position: Position,
    members: Vec<(i64, Position)>,
}

fn snapshot(st: &State, commander: i64, ship: i64) -> Option<Order> {
    let fleet = *st.of.get(&commander)?;
    if !st.is_member(fleet, commander) { return None; }
    let f = st.fleets.get(&fleet)?;
    let leader = f.members.get(&commander)?;
    if !warp::can_command(leader) { return None; }
    let members = f.members.iter().filter_map(|(&id, m)| {
        (id != commander && st.is_member(fleet, id) && m.opt_outs.accepts_fleet_regroups
            && warp::in_scope(leader, m)).then_some((id, Position::from(m)))
    }).collect();
    Some(Order { fleet, commander, ship, position: Position::from(leader), members })
}

fn current(st: &State, order: &Order, pilot: i64, position: Position) -> bool {
    if pilot == order.commander || !st.is_member(order.fleet, order.commander) || !st.is_member(order.fleet, pilot) { return false; }
    let f = &st.fleets[&order.fleet];
    let Some(commander) = f.members.get(&order.commander) else { return false };
    let Some(member) = f.members.get(&pilot) else { return false };
    Position::from(commander) == order.position && Position::from(member) == position
        && member.opt_outs.accepts_fleet_regroups && warp::in_scope(commander, member)
}

fn live(ball: &space::Ball, pilot: i64, ship: i64) -> bool {
    ball.id == ship && ball.char_id == Some(pilot) && ball.owner_id == pilot
        && ball.cloak == 0 && ball.mode != space::MODE_WARP
}

fn enqueue(s: &mut space::System, order: Order) {
    if !s.pilot_ball(order.commander).is_some_and(|b| live(b, order.commander, order.ship)) { return; }
    let Some(point) = s.sample_position(order.ship) else { return };
    if !point.iter().all(|v| v.is_finite()) { return; }
    let order = std::sync::Arc::new(order);
    for &(pilot, position) in &order.members {
        let Some(ship) = s.pilot_ball(pilot).filter(|b| live(b, pilot, b.id)).map(|b| b.id) else { continue };
        let order = order.clone();
        s.command(ship, move |ball, stamp, s| {
            let st = state();
            if !current(&st, &order, pilot, position) || !live(ball, pilot, ship)
                || s.pilot_ball(pilot).is_some()
                || !s.pilot_ball(order.commander).is_some_and(|b| live(b, order.commander, order.ship)) {
                return Vec::new();
            }
            if !ball.goto_point(point) { return Vec::new(); }
            vec![event(stamp, "GotoPoint", vec![Value::Int(ship), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])]
        });
    }
}

pub(in crate::services) async fn regroup(c: &Client) -> CallResult {
    let commander = c.session.int("charid").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    let order = snapshot(&state(), commander, ship).ok_or_else(|| notify_error(messages::fleets::LEADER_REQUIRED))?;
    if let Some(system) = c.session.int("solarsystemid") {
        c.world.with(system, move |s| enqueue(s, order)).await;
    }
    Ok(Value::None)
}

#[cfg(test)]
#[path = "tests/navigation.rs"]
mod tests;
