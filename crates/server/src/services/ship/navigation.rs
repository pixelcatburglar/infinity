//! Point and bookmark navigation and ballpark diagnostics for `michelle.py`.
//! Bookmarks are resolved from the bookmark service; movement uses the system actor.
//! Bubble IDs use the solar system ID, not an original-server bubble algorithm.
use super::*;
use crate::client::filetime_now;
use infinity_core::consts::GM_ROLES;
use crate::services::{bookmarks, fleets, notify_error};

pub(super) fn point_command(b: &mut Ball, t: i64, point: V3) -> Vec<Value> {
    if !b.goto_point(point) { return vec![]; }
    let mut events = vec![event(t, "GotoPoint", vec![Value::Int(b.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])];
    ensure_moving(b, t, &mut events);
    events
}

fn point(c: &Client, point: V3) -> CallResult {
    if !point.iter().all(|v| v.is_finite()) { return Ok(Value::None); }
    movement(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), move |b, t, _| point_command(b, t, point))
}

pub(super) fn goto_point(c: &Client, destination: V3) -> CallResult { point(c, destination) }

async fn destination(c: &Client, id: i64) -> Result<Option<bookmarks::navigation::Destination>, CallError> {
    let destination = bookmarks::navigation::resolve_for_use(c, id).await?;
    if c.session.int("solarsystemid") != Some(destination.system) { return Ok(None); }
    Ok(Some(destination))
}

pub(super) async fn goto_bookmark(c: &Client, id: i64) -> CallResult {
    let Some(destination) = destination(c, id).await? else { return Ok(Value::None) };
    point(c, destination.pos)
}

pub(super) async fn align_bookmark(c: &Client, id: i64) -> CallResult {
    let Some(destination) = destination(c, id).await? else { return Ok(Value::None) };
    let point = destination.pos;
    movement(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), move |b, t, system| {
        let pos = b.position_at(system.now_ms());
        let dir = [point[0] - pos[0], point[1] - pos[1], point[2] - pos[2]];
        if !dir.iter().all(|v| v.is_finite()) || !b.goto_direction(dir) { return vec![]; }
        let mut events = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
        ensure_moving(b, t, &mut events);
        events
    })
}

pub(super) async fn warp_bookmark(c: &Client, id: i64, min_range: f64, fleet: bool) -> CallResult {
    if !min_range.is_finite() { return Ok(Value::None); }
    let Some(destination) = destination(c, id).await? else { return Ok(Value::None) };
    let order = if fleet { Some(fleets::warp::authorize(c).await?) } else { None };
    warp_destination(&Identity::of_client(c), &c.db, &c.world, c.session.int("solarsystemid"), order, WarpDestination::Ordinary { point: destination.pos, object_radius: None, min_range }, None, Value::dict([("bookmarkID", Value::Int(destination.bookmark_id))])).await
}

pub(super) async fn relativity(c: &Client, reference: V3) -> CallResult {
    if !reference.iter().all(|v| v.is_finite()) { return Ok(Value::None); }
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), Identity::of_client(c).ship_id) else { return Ok(Value::None) };
    let Some((pos, tick)) = c.world.with(system, move |s| Some((s.sample_position(ship)?, s.current_time()))).await.flatten() else { return Ok(Value::None) };
    let diff = (pos[0] - reference[0]).hypot(pos[1] - reference[1]).hypot(pos[2] - reference[2]);
    let now = filetime_now();
    Ok(Value::tuple([Value::Float(diff), Value::Float(pos[0]), Value::Float(pos[1]), Value::Float(pos[2]), Value::Int(tick), Value::Int(now), Value::Int(now)]))
}

pub(super) async fn bubble_id(c: &Client) -> CallResult {
    let denied = || notify_error(messages::gm::GM_ROLE_REQUIRED);
    if c.login.role & GM_ROLES == 0 { return Err(denied()); }
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), Identity::of_client(c).ship_id) else { return Err(denied()) };
    if !c.world.with(system, move |s| s.ball(ship).is_some()).await.unwrap_or(false) { return Err(denied()); }
    Ok(Value::Int(system))
}
