//! Undocking, flying and docking through the location-bound `ship` service and
//! system-bound `beyonce` remote ballpark. Destiny updates are consumed by
//! `eve/client/script/remote/michelle.py`; gate destinations are consumed by
//! `eve/client/script/parklife/autopilot.py`. Ship attributes and celestial
//! data come from the SDE; locations are persisted in game tables.
//! Ship shield, armor and hull condition are persisted in item dogma overrides.
//! Selected and unpiloted persistent hulls share fitted ball construction.
//!
//! Movement commands apply at destiny boundaries; jump and docking waits and arrival cloak
//! expiry use the actor tick. During the four-second jump wait, the source ballpark
//! receives jump-out effects before the session changes. In-range docking stops and
//! uncloaks the ship, sends acceptance, then waits 5.5 seconds while forwarding destiny
//! before persisting condition and changing session. Docking and gate ranges, as well as
//! live warp targets, use positions sampled at the system actor's time. GOTO stores an
//! absolute point; direction commands project a point from the current position and send
//! the raw direction.
//! Movement, stop, accepted warp, docking and jumping cancel arrival cloak through the system actor.
//! Carbonengine `Ballpark::WarpTo` (`Ballpark.cpp:4102-4113`) falls back to
//! GotoPoint with no WarpFX when the raw destination is strictly below 100 km;
//! exactly 100 km takes the full-warp path. Exit callbacks are transient and
//! cancelled by replacement movement or restart; collision and bump physics are absent.
//! Local-chat invites use an outside-space delay for session settlement.
//! Ship loss commits before its independently best-effort killmail and insurance
//! operations. NPC workers are outside this module's scope.
//! Completed dock, undock, jump and committed ship loss hand off lifecycle events.
//! Successful gate and wormhole arrivals record destination jumps and visits together;
//! a crash between arrival and the telemetry transaction can leave an arrival unrecorded.

use super::{db_err, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{FILETIME_SECOND, FLAG_HANGAR, TYPE_CAPSULE};
use crate::db::outbox::{dispatch_client, Effect, Outbox};
use crate::lua::events::GameEvent;
use crate::dao::{characters::{ship as character_dao, selection}, items::{ship as item_dao, creation}};
use crate::messages;
use crate::space::{self, event, special_fx, Ball, V3, CLOAK_RESTORE, SPECIAL_FX_UNSPECIFIED_DURATION};
use crate::space::system::Timer;
use marshal::Value;
use tracing::{info, warn};

/// Server gameplay policy: minimum clearance from an object when warping to it (m).
const MIN_OBJECT_WARP_CLEARANCE: f64 = 2500.0;

/// Docking interaction range in metres (`menusvc.DEFAULT_INTERACTION_RANGE`).
const DOCKING_RANGE: f64 = 2500.0;

/// Docking acceptance delay in milliseconds (`appConst.invulnerabilityDocking`).
const DOCK_DELAY_MS: u64 = 5500;

fn required<T>(value: Option<T>, field: &str) -> Result<T, CallError> {
    value.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SDE {field}")))
}

fn ancestry(sde: &crate::sde::Sde, system: i64) -> Result<(i64, i64), CallError> {
    let row = required(sde.map.system(system), "solar system")?;
    Ok((required(row.constellation_id, "constellation ID")?, required(row.region_id, "region ID")?))
}

struct GateRow { dest_solar_system_id: i64, x: f64, y: f64, z: f64, radius: f64 }

fn gate_destination(sde: &crate::sde::Sde, from: i64, to: i64, system: i64) -> Result<Option<GateRow>, CallError> {
    let Some(gate) = sde.map.stargate(from) else { return Ok(None); };
    if gate.solar_system_id != Some(system) || gate.dest_stargate_id != Some(to) { return Ok(None); }
    let Some(dest_system) = gate.dest_solar_system_id else { return Ok(None); };
    if sde.map.system(dest_system).is_none() { return Ok(None); }
    let Some(celestial) = sde.map.celestial(to) else { return Ok(None); };
    if celestial.type_id.and_then(|id| i32::try_from(id).ok()).and_then(|id| sde.types.get(id)).is_none() { return Ok(None); }
    Ok(Some(GateRow {
        dest_solar_system_id: dest_system,
        x: required(celestial.x, "gate x")?, y: required(celestial.y, "gate y")?, z: required(celestial.z, "gate z")?,
        radius: f64::from(sde.celestial_radius(celestial)),
    }))
}


fn f(v: Option<&Value>) -> f64 {
    v.and_then(|v| v.as_float()).unwrap_or(0.0)
}

/// Construct the selected ship using its pilot's fit and live projections.
pub(super) async fn ship_ball(c: &Client, ship_id: i64, pos: [f64; 3]) -> Result<Ball, CallError> {
    balls::selected(c, ship_id, pos).await
}

/// Refuse docking and gate jumps while the pilot's weapons timer runs (crimewatch).
fn check_weapons_timer(c: &Client, what: &str) -> Result<(), CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    match super::crimewatch::weapons_timer(char_id) {
        Some(until) => {
            let secs = ((until - crate::client::filetime_now()) / 10_000_000).max(1);
            Err(CallError::User {
                msg: messages::MSG_CUSTOM_NOTIFY.into(),
                dict: vec![("notify".into(), Value::unicode(messages::weapons_timer_message(secs, what)))],
            })
        }
        None => Ok(()),
    }
}

/// Quiesce condition mutations before removing the ball and capturing its final state.
async fn remove_and_save(c: &Client, system: i64, ship: i64) -> Result<(), CallError> {
    let ball = {
        let _lock = super::market::lock().await;
        c.world.remove_ball(system, ship).await
    }.ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship {ship} is no longer in system {system}")))?;
    if let Err(error) = super::combat::save_condition(&c.db, ship).await {
        if let Err(restore) = c.world.add_ship(system, ball).await {
            tracing::warn!(ship, %restore, "failed to restore ship after condition save failure");
        }
        return Err(error);
    }
    Ok(())
}

async fn dock(c: &mut Client, station: i64) -> CallResult {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else {
        return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
    };
    check_weapons_timer(c, messages::WEAPONS_TIMER_DOCK)?;
    let gap = c.world.with(system, move |s| {
        let (b, station) = (s.ball(ship)?, s.ball(station)?);
        Some(space::dist(b.position_at(s.now_ms()), station.position_at(s.now_ms()))
            - b.radius as f64 - station.radius as f64)
    }).await.flatten();
    if gap.is_none_or(|g| g > DOCKING_RANGE) {
        return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
    }
    c.world.command(system, ship, |b, t, _| {
        b.stop();
        vec![event(t, "Stop", vec![Value::Int(b.id)])]
    });
    c.world.uncloak(system, ship).await;
    c.notify("OnDockingAccepted", vec![Value::Int(station)]);
    if !c.wait_pumping(system, DOCK_DELAY_MS).await.map_err(CallError::Internal)? {
        return Err(CallError::Internal(anyhow::anyhow!("system actor stopped during docking wait")));
    }
    super::combat::reset(c);
    remove_and_save(c, system, ship).await?;
    c.world.unobserve(c.login.client_id);
    item_dao::dock(&c.db, ship, station, FLAG_HANGAR).await.map_err(db_err)?;
    character_dao::dock(&c.db, c.session.int("charid").unwrap_or(0), station, system).await.map_err(db_err)?;
    info!(user = c.login.user_id, "ship {ship} docked at {station}");
    c.change_session(vec![("solarsystemid", Value::None), ("stationid", Value::Int(station))]);
    if let Some(char_id) = c.session.int("charid") {
        let mut outbox = Outbox::default();
        outbox.event(GameEvent::Docked { char_id, ship_id: ship, station_id: station, system_id: system });
        dispatch_client((), outbox, c)?;
    }
    Ok(Value::None)
}

/// Jump range: ship and gate surfaces within this distance (m).
const JUMP_RANGE: f64 = 2500.0;


/// Stargate jump delay (ballparkCommon/jumpdelay.py: appConst.invulnerabilityJumping, ms):
/// the gate animation plays this long before the session moves to the next system.
const JUMP_DELAY_MS: u64 = 4000;
/// Existing server policy: allow the session change to settle before inviting Local chat.
const LOCAL_CHAT_INVITE_DELAY_MS: u64 = 2000;
/// Existing server timing for stopping the wormhole effects.CloakRegardless effect.
const WORMHOLE_EFFECT_STOP_DELAY_MS: u64 = 3000;
/// Arrival cloak duration in seconds (`appConst.invulnerabilityRestoring`).
const JUMP_CLOAK_SECONDS: i64 = 60;
const UE_LOCID: i64 = 3;

/// Install an arriving ship with the actor-managed restoring cloak.
pub(super) async fn arrive_cloaked(c: &mut Client, system: i64, mut ball: Ball) -> Result<i64, CallError> {
    let cloak_until = crate::client::filetime_now() + JUMP_CLOAK_SECONDS * FILETIME_SECOND;
    ball.cloak = CLOAK_RESTORE;
    ball.cloak_until = cloak_until;
    c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    Ok(cloak_until)
}

/// Send the restoring cloak timer after the arrival session change.
pub(super) fn notify_jump_cloak(c: &mut Client, ship: i64, cloak_until: i64) {
    c.notify("OnJumpCloakUpdated", vec![Value::Int(ship), Value::Int(cloak_until), Value::Int(JUMP_CLOAK_SECONDS * FILETIME_SECOND)]);
}

/// CmdStargateJump(fromStargateID, toStargateID, shipID): leave through the gate and
/// arrive, cloaked, next to its destination gate in the neighbouring system.
async fn stargate_jump(c: &mut Client, from_gate: i64, to_gate: i64) -> CallResult {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(CallError::User { msg: messages::MSG_JUMP_GATE_HAS_NO_DESTINATION.into(), dict: vec![] });
    };
    check_weapons_timer(c, messages::WEAPONS_TIMER_JUMP)?;
    let dest = gate_destination(&c.sde, from_gate, to_gate, system)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_JUMP_GATE_HAS_NO_DESTINATION.into(), dict: vec![] })?;
    let dest_system: i64 = dest.dest_solar_system_id;

    // In range of the gate?
    let gap = c.world.with(system, move |s| {
        let (b, gate) = (s.ball(ship)?, s.ball(from_gate)?);
        Some(space::dist(b.position_at(s.now_ms()), gate.position_at(s.now_ms()))
            - gate.radius as f64
            - b.radius as f64)
    }).await.flatten();
    if gap.is_none_or(|g| g > JUMP_RANGE) {
        return Err(CallError::User { msg: messages::MSG_NOT_CLOSE_ENOUGH_TO_JUMP.into(), dict: vec![("maxdist".into(), Value::Int(JUMP_RANGE as i64))] });
    }
    // Arrive a few kilometres off the destination gate, on a heading fixed per gate pair.
    let gate_pos: [f64; 3] = [dest.x, dest.y, dest.z];
    let a = ((from_gate ^ to_gate) % 360) as f64 * std::f64::consts::PI / 180.0;
    let dist = dest.radius + 5000.0;
    let pos = [gate_pos[0] + a.cos() * dist, gate_pos[1], gate_pos[2] + a.sin() * dist];
    let out = Jump { through: from_gate, guid: "effects.JumpOut", other_type: None, dest_system, pos };
    jump_out_and_arrive(c, system, ship, char_id, out).await?;
    info!(user = c.login.user_id, "ship {ship} jumped {from_gate} ({system}) -> {to_gate} ({dest_system})");
    Ok(Value::None)
}

/// A jump out of `system` through `through` (a gate, a wormhole) to `pos` in `dest_system`.
pub(super) struct Jump {
    pub through: i64,
    pub guid: &'static str,
    pub other_type: Option<i64>,
    pub dest_system: i64,
    pub pos: V3,
}

/// Leave through a gate or wormhole: play the jump-out effect where everyone sees it, wait
/// out the animation, then arrive cloaked in the destination system and move the session.
pub(super) async fn jump_out_and_arrive(c: &mut Client, system: i64, ship: i64, char_id: i64, jump: Jump) -> Result<(), CallError> {
    let Jump { through, guid, other_type, dest_system, pos } = jump;
    let (constellation, region) = ancestry(&c.sde, dest_system)?;
    c.world.system(dest_system).await.map_err(CallError::Internal)?;
    // Room on the other side, or a place in its jump queue.
    if let Err(position) = c.world.request_entry(dest_system, char_id).await {
        info!(user = c.login.user_id, "jump into {dest_system} queued at {position}");
        let msg = if position == 0 { "SystemCheck_JumpFailed_Loading" } else { "SystemCheck_JumpFailed_Queued" };
        return Err(CallError::User {
            msg: msg.into(),
            dict: vec![("system".into(), Value::tuple([Value::Int(UE_LOCID), Value::Int(dest_system)])), ("position".into(), Value::Int(position.max(1)))],
        });
    }

    // Jump out where everyone can see it, and let the gate animation run.
    c.world.uncloak(system, ship).await;
    c.world.command(system, ship, move |b, t, _| {
        b.stop();
        vec![
            event(t, "Stop", vec![Value::Int(b.id)]),
            special_fx(t, b.id, Some(through), other_type, guid, 1, 0, SPECIAL_FX_UNSPECIFIED_DURATION, Some(Value::tuple([Value::Int(dest_system)]))),
        ]
    });
    if !c.wait_pumping(system, JUMP_DELAY_MS).await.map_err(CallError::Internal)? {
        return Err(CallError::Internal(anyhow::anyhow!("source system actor stopped during jump wait")));
    }

    super::combat::reset(c);
    remove_and_save(c, system, ship).await?;
    c.world.unobserve(c.login.client_id);
    item_dao::position(&c.db, ship, dest_system, pos).await.map_err(db_err)?;
    character_dao::location(&c.db, char_id, dest_system, constellation, region).await.map_err(db_err)?;
    let ball = ship_ball(c, ship, pos).await?;
    let cloak_until = arrive_cloaked(c, dest_system, ball).await?;
    // The client rebinds its remote ballpark for the new system, which sends SetState.
    c.change_session(vec![
        ("solarsystemid", Value::Int(dest_system)),
        ("solarsystemid2", Value::Int(dest_system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
    ]);
    notify_jump_cloak(c, ship, cloak_until);
    // The client leaves the old Local itself; the new one arrives as an invite, sent once
    // the session change has had time to land.
    let chat = c.chat.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(LOCAL_CHAT_INVITE_DELAY_MS)).await;
        chat.invite(char_id, &format!("local_{dest_system}"));
    });
    // effects.JumpOutWormhole hides the jumping ship with a repeating client-side
    // CloakRegardless effect; it ends only when told to, once the pilot sees the new system.
    if guid == "effects.JumpOutWormhole" {
        c.world.schedule(dest_system, WORMHOLE_EFFECT_STOP_DELAY_MS, move |s| {
            s.broadcast(
                ship,
                vec![special_fx(s.current_time(), ship, None, None, "effects.CloakRegardless", 0, 0, SPECIAL_FX_UNSPECIFIED_DURATION, None)],
            );
        });
    }
    super::map::events::arrived(&c.db, char_id, dest_system, crate::client::filetime_now()).await;
    let mut outbox = Outbox::default();
    outbox.event(GameEvent::Jumped { char_id, ship_id: ship, from_system_id: system, to_system_id: dest_system });
    dispatch_client((), outbox, c)?;
    Ok(())
}


/// GM teleport (/tr): put the ship at `pos` in `dest_system` at once. Within the system
/// the client gets a fresh ballpark; elsewhere the session moves as on a jump.
pub(super) async fn teleport(c: &mut Client, dest_system: i64, pos: V3) -> Result<(), CallError> {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::MUST_BE_IN_SPACE))] });
    };
    c.world.system(dest_system).await.map_err(CallError::Internal)?;
    super::combat::reset(c);
    remove_and_save(c, system, ship).await?;
    item_dao::position(&c.db, ship, dest_system, pos).await.map_err(db_err)?;
    let ball = ship_ball(c, ship, pos).await?;
    if dest_system == system {
        c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
        return send_state(c).await;
    }
    let (constellation, region) = ancestry(&c.sde, dest_system)?;
    c.world.unobserve(c.login.client_id);
    character_dao::location(&c.db, char_id, dest_system, constellation, region).await.map_err(db_err)?;
    c.world.add_ship(dest_system, ball).await.map_err(CallError::Internal)?;
    c.change_session(vec![
        ("solarsystemid", Value::Int(dest_system)),
        ("solarsystemid2", Value::Int(dest_system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
    ]);
    let chat = c.chat.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(LOCAL_CHAT_INVITE_DELAY_MS)).await;
        chat.invite(char_id, &format!("local_{dest_system}"));
    });
    info!(user = c.login.user_id, "ship {ship} teleported {system} -> {dest_system}");
    Ok(())
}

/// The pilot's ship was destroyed in space (combat::damage_player): its fittings and
/// cargo go with it and the pilot ejects in a capsule where it died.
pub async fn lose_ship(c: &mut Client, ship: i64, pos: [f64; 3]) -> Result<(), CallError> {
    let (Some(char_id), Some(system)) = (c.session.int("charid"), c.session.int("solarsystemid")) else { return Ok(()) };
    if c.session.int("shipid") != Some(ship) {
        return Ok(());
    }
    let corp = c.session.int("corpid").unwrap_or(0);
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        |tx, outbox: &mut Outbox<()>| {
            let sde = sde.clone();
            Box::pin(async move {
            if !item_dao::lock_reload_ship(tx, ship).await.map_err(db_err)? {
                return Ok(None);
            }
            let lost_type = crate::dao::items::inventory::type_id(tx, ship).await.map_err(db_err)?;
            // The killmail lists what went down with it: fittings, cargo and loaded charges.
            let mut lost: super::crimewatch::Lost = item_dao::lost_items(tx, ship).await.map_err(db_err)?
                .iter().map(|r| (r.type_id, r.flag_id, r.singleton, if r.singleton { 1 } else { r.quantity as i64 })).collect();
            for r in item_dao::lost_charges(tx, ship).await.map_err(db_err)? {
                lost.push((r.type_id, r.flag_id, false, r.quantity as i64));
            }
            let home = if lost_type == Some(TYPE_CAPSULE) {
                let r = super::character::home_station::home(tx, &sde, char_id).await?;
                Some((r.station_id, r.solar_system_id, r.constellation_id, r.region_id))
            } else { None };
            item_dao::delete_lost(tx, ship).await.map_err(db_err)?;
            let capsule: i64 = if let Some((station, _, _, _)) = home {
                creation::rookie_ship(tx, TYPE_CAPSULE, char_id, station, FLAG_HANGAR).await.map_err(db_err)?
            } else {
                crate::dao::items::creation::positioned(tx, TYPE_CAPSULE, char_id, system, pos).await.map_err(db_err)?
            };
            if let Some((station, system, constellation, region)) = home {
                character_dao::set_home(tx, char_id, capsule, station, system, constellation, region).await.map_err(db_err)?;
            } else {
                selection::set_ship(tx, char_id, capsule).await.map_err(db_err)?;
            }
            outbox.operation(());
            if let Some(type_id) = lost_type {
                outbox.event(GameEvent::ShipLost { char_id, ship_id: ship, type_id, system_id: system });
            }
            Ok(Some((capsule, home, lost_type, lost)))
            })
        },
        |result, outbox| async move {
            let Some((capsule, home, lost_type, lost)) = result else { return Ok(()) };
            for entry in outbox.into_entries() {
                match entry {
                    Effect::Notification(notification) => notification.dispatch(&c.world.clone(), Some(&mut *c))?,
                    Effect::Event(event) => crate::lua::on_event(event),
                    Effect::Operation(()) => {
                        super::combat::reset(c);
                        super::combat::forget_ship(ship);
                        if let Some(ship_type) = lost_type {
                            match super::crimewatch::ship_lost(&c.db, &c.world, system, char_id, corp, ship, ship_type, &lost).await {
                                Err(CallError::Internal(e)) => warn!(user = c.login.user_id, "killmail for {ship}: {e:#}"),
                                Err(CallError::User { msg, .. }) => warn!(user = c.login.user_id, "killmail for {ship}: {msg}"),
                                Ok(()) => {}
                            }
                            match super::insurance::ship_destroyed(&c.db, &c.world, ship).await {
                                Err(CallError::Internal(e)) => warn!(user = c.login.user_id, "insurance payout for {ship}: {e:#}"),
                                Err(CallError::User { msg, .. }) => warn!(user = c.login.user_id, "insurance payout for {ship}: {msg}"),
                                Ok(()) => {}
                            }
                        }
                    }
                }
            }
            if let Some((station, system, constellation, region)) = home {
                c.world.unobserve(c.login.client_id);
                info!(user = c.login.user_id, "{char_id} podded; new clone at {station}");
                c.change_session(vec![
                    ("solarsystemid", Value::None),
                    ("stationid", Value::Int(station)),
                    ("solarsystemid2", Value::Int(system)),
                    ("constellationid", Value::Int(constellation)),
                    ("regionid", Value::Int(region)),
                    ("shipid", Value::Int(capsule)),
                ]);
                Ok(())
            } else {
                let ball = ship_ball(c, capsule, pos).await?;
                c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
                info!(user = c.login.user_id, "ship {ship} lost; {char_id} ejected in capsule {capsule}");
                c.change_session(vec![("shipid", Value::Int(capsule))]);
                send_state(c).await
            }
        },
    ).await
}

/// Push the full destiny state for the client's current system (on beyonce bind / UpdateStateRequest).
pub async fn send_state(c: &mut Client) -> Result<(), CallError> {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(()) };
    let set_state = c.world.observe(system, c.login.client_id, ship, c.destiny_tx.clone()).await.map_err(CallError::Internal)?;
    c.notify("DoDestinyUpdate", vec![Value::list([set_state]), Value::Bool(false)]);
    Ok(())
}

/// Steer or stop the ship, dropping a jump cloak.
fn movement(c: &mut Client, f: impl FnOnce(&mut Ball, i64, &space::System) -> Vec<Value> + Send + 'static) -> CallResult {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(Value::None) };
    c.world.post(system, move |s| {
        s.drop_jump_cloak(ship);
        s.command(ship, f);
    });
    Ok(Value::None)
}

/// Warp the session's ship to `point`. With `object` (position, radius) it lands
/// `min_range` (at least 2.5 km) off that object's surface on the near side; without,
/// `min_range` short of the point itself (bookmarks, mission sites, warp-in points),
/// as Ballpark::RealWarp places it.
pub(super) fn valid_warp_geometry(object: Option<(V3, f64)>, point: V3, min_range: f64) -> bool {
    point.iter().all(|v| v.is_finite()) && min_range.is_finite()
        && object.is_none_or(|(pos, radius)| pos.iter().all(|v| v.is_finite()) && radius.is_finite())
}

pub(super) async fn warp_to(c: &mut Client, object: Option<(V3, f64)>, point: V3, min_range: f64, on_exit: Option<Timer>) -> CallResult {
    if !valid_warp_geometry(object, point, min_range) { return Ok(Value::None); }
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(Value::None) };
    let pilot = c.session.int("charid").unwrap_or(0);
    warp_pilot(&c.db, &c.world, system, pilot, ship, object, point, min_range, on_exit, None).await?;
    Ok(Value::None)
}

pub(super) async fn warp_destination(c: &mut Client, order: Option<super::fleets::warp::Order>,
    object: Option<(V3, f64)>, point: V3, min_range: f64, on_exit: Option<Timer>, destination: Value) -> CallResult {
    match order {
        Some(order) => super::fleets::warp::execute(c, order, object, point, min_range, on_exit, destination).await,
        None => warp_to(c, object, point, min_range, on_exit).await,
    }
}

/// Load and fence each pilot's own fit; preserve the callback across stale revisions.
pub(super) async fn warp_pilot(db: &crate::db::Db, world: &space::World, system: i64, pilot: i64, ship: i64,
    object: Option<(V3, f64)>, point: V3, min_range: f64, mut on_exit: Option<Timer>,
    order: Option<std::sync::Arc<super::fleets::warp::Order>>) -> Result<(bool, Option<Timer>), CallError> {
    loop {
        let fit = super::fit::Fit::load_in_space(db, world, system, pilot, ship).await?;
        let Some(revision) = fit.projection_revision() else { return Ok((false, on_exit)) };
        let scrambled = fit.value(super::fit::Key::Ship, crate::consts::dogma::ATTR_WARP_SCRAMBLE_STATUS) > 0.0;
        let is_fleet = order.is_some();
        let guard = order.clone();
        let result = world.with(system, move |s| {
            let mut callback = on_exit;
            let result = if let Some(order) = guard {
                super::fleets::warp::commit(s, &order, pilot, ship, |s| {
                    commit_warp(s, ship, revision, scrambled, object, point, min_range, callback.take())
                })
            } else {
                Some(commit_warp(s, ship, revision, scrambled, object, point, min_range, callback.take()))
            };
            match result {
                Some((current, returned)) => (Some(current), returned),
                None => (None, callback),
            }
        }).await;
        let Some((current, callback)) = result else { return Ok((false, None)) };
        on_exit = callback;
        let Some(current) = current else { return Ok((false, on_exit)) };
        if !current { continue; }
        if scrambled {
            if is_fleet { return Ok((false, on_exit)); }
            return Err(super::notify_error(messages::effects::WARP_SCRAMBLED));
        }
        return Ok((true, on_exit));
    }
}

pub(super) fn commit_warp(s: &mut space::System, ship: i64, revision: u64, scrambled: bool,
    object: Option<(V3, f64)>, point: V3, min_range: f64, on_exit: Option<Timer>) -> (bool, Option<Timer>) {
    if s.projection_revision(ship) != revision || scrambled {
        return (s.projection_revision(ship) == revision, on_exit);
    }
    s.drop_jump_cloak(ship);
    s.clear_warp_exit(ship);
    s.command(ship, move |b, t, _| warp_command(b, t, object, point, min_range));
    if let Some(callback) = on_exit {
        s.on_warp_exit(ship, callback);
    }
    (true, None)
}

fn warp_command(b: &mut Ball, t: i64, object: Option<(V3, f64)>, point: V3, min_range: f64) -> Vec<Value> {
    let stand_off = match object {
        Some((_, radius)) => radius + b.radius as f64 + min_range.max(MIN_OBJECT_WARP_CLEARANCE),
        None => min_range.max(0.0),
    };
    let factor = b.warp_factor.max(1);
    match b.warp_to(point, stand_off, factor) {
        space::WarpOutcome::Ignored => vec![],
        space::WarpOutcome::Approach => vec![event(t, "GotoPoint", vec![Value::Int(b.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])],
        space::WarpOutcome::Align => {
            b.set_speed_fraction(1.0);
            vec![
                event(t, "WarpTo", vec![Value::Int(b.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2]), Value::Float(stand_off), Value::Int(factor)]),
                event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]),
                special_fx(t, b.id, None, None, "effects.Warping", 1, 0, SPECIAL_FX_UNSPECIFIED_DURATION, None),
            ]
        }
    }
}

/// Ensure a stationary ship starts moving when given a heading (client sends 1.0 separately too).
fn ensure_moving(b: &mut Ball, tick: i64, ev: &mut Vec<Value>) {
    if b.speed_fraction <= 0.0 {
        b.set_speed_fraction(1.0);
        ev.push(event(tick, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]));
    }
}

pub(crate) mod balls;
mod ship;
mod beyonce;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "ship" => ship::dispatch(c, call).await,
        "beyonce" => beyonce::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
