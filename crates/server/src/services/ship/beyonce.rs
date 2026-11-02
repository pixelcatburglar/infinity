//! The beyonce remote ballpark service uses the space actor and SDE gate data.
//! Belt rats spawn after actual warp exit; pending callbacks do not survive cancellation or restart.
//! Explicit stop and movement commands cancel an arrival jump cloak.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let a = call.args;
    Some(match (call.service, call.method) {
        ("beyonce", "UpdateStateRequest") => send_state(c).await.map(|_| Value::None),
        ("beyonce", "CmdGotoDirection") => {
            let dir = [f(a.first()), f(a.get(1)), f(a.get(2))];
            if !dir.iter().all(|v| v.is_finite()) { return Some(Ok(Value::None)); }
            movement(c, move |b, t, _| {
                if !b.goto_direction(dir) { return vec![]; }
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdAlignTo") => {
            let target = call.kwargs.get("dstID").and_then(|v| v.as_int()).or_else(|| a.first().and_then(|v| v.as_int()));
            movement(c, move |b, t, others| {
                let Some(tp) = target.and_then(|id| others.ball(id)).map(|o| o.pos) else { return vec![] };
                let dir = [tp[0] - b.pos[0], tp[1] - b.pos[1], tp[2] - b.pos[2]];
                if !b.goto_direction(dir) { return vec![]; }
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdStop") => {
            movement(c, |b, t, _| {
                b.stop();
                vec![event(t, "Stop", vec![Value::Int(b.id)])]
            })
        }
        ("beyonce", "CmdSetSpeedFraction") => {
            let frac = f(a.first()) as f32;
            if !frac.is_finite() { return Some(Ok(Value::None)); }
            movement(c, move |b, t, _| {
                b.set_speed_fraction(frac);
                vec![event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(b.speed_fraction as f64)])]
            })
        }
        ("beyonce", "CmdFollowBall") | ("beyonce", "CmdOrbit") => {
            let target = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let range = f(a.get(1)) as f32;
            let orbit = call.method == "CmdOrbit";
            if !range.is_finite() { return Some(Ok(Value::None)); }
            movement(c, move |b, t, others| {
                let Some(target_ball) = others.ball(target) else { return vec![] };
                if !(if orbit { b.orbit(target_ball, range) } else { b.follow_ball(target_ball, range) }) { return vec![]; }
                let name = if orbit { "Orbit" } else { "FollowBall" };
                let mut ev = vec![event(t, name, vec![Value::Int(b.id), Value::Int(target), Value::Float(range as f64)])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdWarpToStuff") if a.first().and_then(|v| v.as_text()).as_deref() == Some("scan") => {
            let target = a.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            let order = if call.kwargs.get("fleet").is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0)) {
                match super::super::fleets::warp::authorize(c).await { Ok(order) => Some(order), Err(err) => return Some(Err(err)) }
            } else { None };
            crate::services::exploration::warp_to_scan(c, &target, min_range, order).await
        }
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot") => {
            let mut order = if call.kwargs.get("fleet").is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0)) {
                match super::super::fleets::warp::authorize(c).await { Ok(order) => Some(order), Err(err) => return Some(Err(err)) }
            } else { None };
            let target = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            // 'char': a fleet member's ship.
            let by_pilot = a.first().and_then(|v| v.as_text()).as_deref() == Some("char");
            let me = c.session.int("charid").unwrap_or(0);
            if by_pilot && order.as_ref().is_some_and(|order| !super::super::fleets::warp::permits_target(order, target)) {
                return Some(Ok(Value::None));
            }
            let found = if let Some(sys) = c.session.int("solarsystemid") {
                c.world.with(sys, move |s| {
                    let b = if by_pilot { s.pilot_ball(target) } else { s.ball(target) }?;
                    if !s.fleet_broadcast_visible(me, b.id)
                        || (by_pilot && (b.char_id != Some(target) || b.owner_id != target)) { return None; }
                    Some((b.id, b.position_at(s.now_ms()), b.radius as f64, s.warp_in(b.id)))
                }).await.flatten()
            } else {
                None
            };
            let Some((id, pos, radius, warp_in)) = found else {
                return Some(Ok(Value::None));
            };
            if by_pilot {
                if let Some(order) = order.as_mut() { super::super::fleets::warp::target(order, target, id); }
            }
            let on_exit: Option<Timer> = if !by_pilot {
                c.session.int("solarsystemid").map(|system| {
                    let db = c.db.clone();
                    let world = std::sync::Arc::downgrade(&c.world);
                    let user = c.login.user_id;
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
            let destination = if !by_pilot && c.sde.map.celestial(id).is_some() {
                Value::dict([("celestialID", Value::Int(id))])
            } else { Value::Dict(vec![]) };
            match warp_in {
                Some(point) => warp_destination(c, order, None, point, min_range, on_exit, destination).await,
                None => warp_destination(c, order, Some((pos, radius)), pos, min_range, on_exit, destination).await,
            }
        }
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
                    if let Some(event) = change.event {
                        let mut outbox = Outbox::default();
                        outbox.event(event);
                        dispatch_client((), outbox, c).map(|_| Value::None)
                    } else {
                        Ok(Value::None)
                    }
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


