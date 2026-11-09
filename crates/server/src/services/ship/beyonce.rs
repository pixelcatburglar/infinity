//! The beyonce remote ballpark service uses the space actor and SDE gate data.
//! Belt rats spawn after actual warp exit; pending callbacks do not survive cancellation or restart.
//! Explicit stop and movement commands cancel an arrival jump cloak.
//! Warp target visibility comes from the space actor, independently of client observers.
//! Point and bookmark navigation and diagnostics use the current system actor;
//! bookmark access is resolved by the bookmark service.
//! `michelle.py` receives a refusal for tutorial site positions; tutorial instances
//! are not modelled and that query uses no data or space state.
use super::*;
use crate::client::short;
use tracing::warn;

fn wants_fleet(call: &Call<'_>) -> bool {
    call.kwargs.get("fleet").is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0))
}

async fn fleet_order(c: &Client, call: &Call<'_>) -> Result<Option<super::super::fleets::warp::Order>, CallError> {
    if wants_fleet(call) {
        super::super::fleets::warp::authorize(c).await.map(Some)
    } else {
        Ok(None)
    }
}
fn lifecycle_args<const N: usize>(c: &Client, call: &Call<'_>) -> Result<[i64; N], CallError> {
    let invalid = || super::super::notify_error(infinity_core::messages::STRUCTURE_OPERATION_INVALID);
    let system = c.session.int("solarsystemid").ok_or_else(invalid)?;
    if !call.bound || c.session.int("stationid").is_some()
        || !matches!(call.bind_params, Some(Value::Int(id)) if *id == system)
        || !(matches!(call.kwargs, Value::None) || matches!(call.kwargs, Value::Dict(v) if v.is_empty()))
        || call.args.len() != N { return Err(invalid()); }
    let mut ids = [0; N];
    for (id, value) in ids.iter_mut().zip(call.args) {
        match value { Value::Int(value) if *value > 0 => *id = *value, _ => return Err(invalid()) }
    }
    Ok(ids)
}

async fn lifecycle_command(c: &Client, call: &Call<'_>) -> CallResult {
    let resources = crate::services::sovereignty::Resources { db: c.db.clone(), sde: c.sde.clone(), world: c.world.clone() };
    let identity = Identity::of_client(c);
    match call.method {
        "CmdTransferCorpAssetOwnership" => {
            let [item, old_owner, new_owner] = lifecycle_args(c, call)?;
            crate::services::sovereignty::lifecycle::transfer(resources, identity, item, old_owner, new_owner).await?;
        }
        _ => {
            let [item] = lifecycle_args(c, call)?;
            crate::services::sovereignty::lifecycle::destruct(resources, identity, item, call.method == "CmdSelfDestructStructure").await?;
        }
    }
    Ok(Value::None)
}


pub(crate) fn stop(identity: &Identity, world: &space::World, space_system: Option<i64>) -> CallResult {
    movement(identity, world, space_system, |b, t, _| {
        b.stop();
        vec![event(t, "Stop", vec![Value::Int(b.id)])]
    })
}

pub(crate) fn orbit(identity: &Identity, world: &space::World, space_system: Option<i64>, target: i64, range: f32) -> CallResult {
    follow_or_orbit(identity, world, space_system, target, range, true)
}

pub(crate) fn follow_ball(identity: &Identity, world: &space::World, space_system: Option<i64>, target: i64, range: f32) -> CallResult {
    follow_or_orbit(identity, world, space_system, target, range, false)
}

fn follow_or_orbit(identity: &Identity, world: &space::World, space_system: Option<i64>, target: i64, range: f32, orbit: bool) -> CallResult {
    movement(identity, world, space_system, move |b, t, others| {
        let Some(target_ball) = others.ball(target) else { return vec![] };
        if !(if orbit { b.orbit(target_ball, range) } else { b.follow_ball(target_ball, range) }) { return vec![]; }
        let name = if orbit { "Orbit" } else { "FollowBall" };
        let mut ev = vec![event(t, name, vec![Value::Int(b.id), Value::Int(target), Value::Float(range as f64)])];
        ensure_moving(b, t, &mut ev);
        ev
    })
}

/// `login_user` is the owning server account ID: the belt-arrival callback forwards it
/// to `belt_rats` for its spawn info log and uses it in the callback failure warning.
pub(crate) async fn warp_to_ball(
    identity: &Identity, db: &crate::db::Db, world: &std::sync::Arc<space::World>, sde: &crate::sde::Sde,
    space_system: Option<i64>, login_user: i64,
    target: i64, by_pilot: bool, min_range: f64, mut order: Option<super::super::fleets::warp::Order>,
) -> CallResult {
    let me = identity.char_id;
    if by_pilot && order.as_ref().is_some_and(|order| !super::super::fleets::warp::permits_target(order, target)) {
        return Ok(Value::None);
    }
    let found = if let Some(sys) = space_system {
        world.with(sys, move |s| {
            let b = if by_pilot { s.pilot_ball(target) } else { s.ball(target) }?;
            if !s.visible_to(me, b.id)
                || (by_pilot && (b.char_id != Some(target) || b.owner_id != target)) { return None; }
            Some((b.id, b.position_at(s.now_ms()), b.radius as f64, s.warp_in(b.id)))
        }).await.flatten()
    } else {
        None
    };
    let Some((id, pos, radius, warp_in)) = found else {
        return Ok(Value::None);
    };
    if by_pilot {
        if let Some(order) = order.as_mut() { super::super::fleets::warp::target(order, target, id); }
    }
    let on_exit: Option<Timer> = if !by_pilot {
        space_system.map(|system| {
            let db = db.clone();
            let world = std::sync::Arc::downgrade(world);
            let user = login_user;
            Box::new(move |s: &mut space::System| {
                s.defer(async move {
                    let Some(world) = world.upgrade() else { return };
                    if let Err(e) = crate::services::exploration::belt_rats(&db, &world, user, system, id, pos).await {
                        warn!(user, "belt rats at {}: {}", id, crate::services::exploration::err_text(e));
                    }
                });
            }) as Timer
        })
    } else { None };
    // Suns, planets, moons and belts land at their warp-in point, not off the surface.
    let destination = if !by_pilot && sde.map.celestial(id).is_some() {
        Value::dict([("celestialID", Value::Int(id))])
    } else { Value::Dict(vec![]) };
    match warp_in {
        Some(point) => warp_destination(identity, db, world, space_system, order, WarpDestination::Ordinary { point, object_radius: None, min_range }, on_exit, destination).await,
        None => warp_destination(identity, db, world, space_system, order, WarpDestination::Ordinary { point: pos, object_radius: Some(radius), min_range }, on_exit, destination).await,
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let a = call.args;
    Some(match (call.service, call.method) {
        ("beyonce", "CmdTransferCorpAssetOwnership" | "CmdSelfDestructStructure" | "CmdAbortSelfDestructStructure") => lifecycle_command(c, call).await,
        ("beyonce", "GetOperationSpawnpointPosition") => Err(super::super::notify_error(infinity_core::messages::npe::OPERATION_SITE_UNAVAILABLE)),
        ("beyonce", "BookmarkLocation") => super::super::bookmarks::beyonce::space_location(c, call).await,
        ("beyonce", "BookmarkScanResult") => super::super::bookmarks::beyonce::scan_result(c, call).await,
        // fleetSvc.py:GetFleetLocationAndInfo reads each member's pos as an (x, y, z) tuple.
        ("beyonce", "GetFleetLocationAndInfo") => super::super::fleets::space_state::locations(c).await,
        // michelle.py:GetFleetState caches the state; fleetSvc.py reads its targetTags.
        ("beyonce", "GetFleetState") => super::super::fleets::space_state::get(c).await,
        // menusvc.py:TagItem sends (itemID, tag); a None tag clears via OnFleetStateChange.
        ("beyonce", "CmdFleetTagTarget") => super::super::fleets::space_state::tag(c, call).await,
        ("beyonce", "UpdateStateRequest") => send_state(c).await.map(|_| Value::None),
        ("beyonce", "CmdGotoPoint") => navigation::goto_point(c, [f(a.first()), f(a.get(1)), f(a.get(2))]),
        ("beyonce", "CmdGotoBookmark") => navigation::goto_bookmark(c, a.first().and_then(|v| v.as_int()).unwrap_or(0)).await,
        ("beyonce", "CmdFleetRegroup") => super::super::fleets::navigation::regroup(c).await,
        ("beyonce", "AdminGetBubbleID") => navigation::bubble_id(c).await,
        ("beyonce", "GetRelativity") => navigation::relativity(c, [f(a.first()), f(a.get(1)), f(a.get(2))]).await,
        ("beyonce", "CmdGotoDirection") => {
            let dir = [f(a.first()), f(a.get(1)), f(a.get(2))];
            if !dir.iter().all(|v| v.is_finite()) { return Some(Ok(Value::None)); }
            movement(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), move |b, t, _| {
                if !b.goto_direction(dir) { return vec![]; }
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdAlignTo") => {
            if let Some(id) = call.kwargs.get("bookmarkID").and_then(|v| v.as_int()) {
                return Some(navigation::align_bookmark(c, id).await);
            }
            let target = call.kwargs.get("dstID").and_then(|v| v.as_int()).or_else(|| a.first().and_then(|v| v.as_int()));
            movement(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), move |b, t, others| {
                let Some(tp) = target.and_then(|id| others.ball(id)).map(|o| o.pos) else { return vec![] };
                let dir = [tp[0] - b.pos[0], tp[1] - b.pos[1], tp[2] - b.pos[2]];
                if !b.goto_direction(dir) { return vec![]; }
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdStop") => stop(&Identity::of_client(c), &c.world, c.session.int("solarsystemid")),
        ("beyonce", "CmdSetSpeedFraction") => {
            let frac = f(a.first()) as f32;
            if !frac.is_finite() { return Some(Ok(Value::None)); }
            movement(&Identity::of_client(c), &c.world, c.session.int("solarsystemid"), move |b, t, _| {
                b.set_speed_fraction(frac);
                vec![event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(b.speed_fraction as f64)])]
            })
        }
        ("beyonce", "CmdFollowBall") | ("beyonce", "CmdOrbit") => {
            let target = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let range = f(a.get(1)) as f32;
            if !range.is_finite() { return Some(Ok(Value::None)); }
            let identity = Identity::of_client(c);
            let system = c.session.int("solarsystemid");
            if call.method == "CmdOrbit" {
                orbit(&identity, &c.world, system, target, range)
            } else {
                follow_ball(&identity, &c.world, system, target, range)
            }
        }
        ("beyonce", "CmdWarpToStuff") if a.first().and_then(|v| v.as_text()).as_deref() == Some("scan") => {
            let target = a.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            let order = match fleet_order(c, call).await { Ok(order) => order, Err(err) => return Some(Err(err)) };
            crate::services::exploration::warp_to_scan(c, &target, min_range, order).await
        }
        ("beyonce", "CmdWarpToStuff") if a.first().and_then(|v| v.as_text()).as_deref() == Some("bookmark") => {
            let id = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            let fleet = wants_fleet(call);
            navigation::warp_bookmark(c, id, min_range, fleet).await
        }
        ("beyonce", "CmdWarpToStuffAutopilot") if a.len() == 1 => {
            let target = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let order = match fleet_order(c, call).await { Ok(order) => order, Err(err) => return Some(Err(err)) };
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            warp_to_ball(&Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"),
                c.login.user_id, target, false, min_range, order).await
        }
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot")
            if matches!(a.first().and_then(|v| v.as_text()).as_deref(), Some("item" | "char")) => {
            let order = match fleet_order(c, call).await { Ok(order) => order, Err(err) => return Some(Err(err)) };
            let target = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            // 'char': a fleet member's ship.
            let by_pilot = a.first().and_then(|v| v.as_text()).as_deref() == Some("char");
            warp_to_ball(&Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"),
                c.login.user_id, target, by_pilot, min_range, order).await
        }
        // Other subject kinds require their own authoritative destination resolver.
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot") => Ok(Value::None),
        ("beyonce", "CmdStargateJump") => {
            let from_gate = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let to_gate = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            stargate_jump(c, from_gate, to_gate).await
        }
        ("beyonce", "CmdDock") | ("beyonce", "CmdTurboDock") => {
            let station = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let identity = Identity::of_client(c);
            let db = c.db.clone();
            let world = c.world.clone();
            let space_system = c.session.int("solarsystemid");
            match dock(&identity, &db, &world, space_system, station, Some(c)).await {
                Ok(change) => {
                    info!(user = c.login.user_id, "ship {} docked at {station}", identity.ship_id.unwrap());
                    c.change_session(change.updates);
                    Ok(Value::None)
                }
                Err(error) => Err(error),
            }
        }
        ("beyonce", m) => {
            warn!(user = c.login.user_id, "UNHANDLED {}::{m} args={} kwargs={}", call.service, short(&marshal::Value::Tuple(a.to_vec())), short(call.kwargs));
            Ok(Value::None)
        }
        _ => return None,
    })
}


