//! Undocking, flying and docking through the location-bound `ship` service and
//! system-bound `beyonce` remote ballpark. Destiny updates are consumed by
//! `eve/client/script/remote/michelle.py`; gate destinations are consumed by
//! `eve/client/script/parklife/autopilot.py`. Ship attributes and celestial
//! data come from the SDE; locations are persisted in game tables.
//! Ship shield, armor and hull condition are persisted in item dogma overrides.
//! Selected and unpiloted persistent hulls share fitted ball construction.
//! Warp requests freeze effective fitted speed with their scramble snapshot. Capacitor
//! pricing uses the current actor fit and mass at activation; a partial debit shortens
//! only that ship's route. Only a full arrival completes its destination callback.
//! Corruption warp-speed increases are not modeled. Queued warp movement is discarded
//! if the ship's persistent or combat membership generation changes before the boundary.
//! NPC undocks use static graphic exit channels and fitted ship movement values;
//! locator rotations and Upwell-specific undock behavior are not modeled.
//! `eve/client/script/ui/services/shipConfigSvc.py` reads persistent owner-bound
//! bay settings; shared-bay ancestry uses current fleet and corporation authority.
//! Physical hull dirt timestamps persist independently of owner-bound bay settings;
//! the cleaning service does not implement a wallet charge. Cold ball preparation
//! reads the same shared timestamp, and a committed change republishes the live
//! slim through the owning system actor without a dedicated notification.
//! Hull player-kill counts persist per physical hull; the client model clamps
//! the display and nothing on the server attributes kills automatically.
//! Stored ships retain their contents and condition; boarding leaves the old hull
//! unpiloted in space, while storing replaces it with a capsule. Launch placement
//! is server-selected surface-tangent geometry, not a captured retail offset.
//! Ejecting leaves the hull unpiloted in space and puts the pilot in a capsule at its position.
//!
//! Movement commands apply at destiny boundaries; jump and docking waits and arrival cloak
//! expiry use the actor tick. During the four-second jump wait, the source ballpark
//! receives jump-out effects before the session changes. In-range docking stops and
//! uncloaks the ship, sends acceptance, then waits 5.5 seconds while forwarding destiny
//! before persisting condition and changing session. Entosis links prevent warp, docking
//! and jumps until their actor-owned cleanup. Docking and gate ranges, as well as
//! live warp targets, use positions sampled at the system actor's time. GOTO stores an
//! absolute point; direction commands project a point from the current position and send
//! the raw direction. Accepting a dock or a gate or wormhole jump also cancels
//! the missiles still inbound on that ship; the ship's own missiles keep their targets.
//! A positive gate scramble status refuses a stargate jump after the range check.
//! Movement, stop, accepted warp, docking and jumping cancel arrival cloak through the system actor.
//! Carbonengine `Ballpark::WarpTo` (`Ballpark.cpp:4102-4113`) falls back to
//! GotoPoint with no WarpFX when the raw destination is strictly below 100 km;
//! exactly 100 km takes the full-warp path. Exit callbacks are transient and
//! cancelled by replacement movement or restart; collision and bump physics are absent.
//! Local-chat invites use an outside-space delay for session settlement.
//! NPC workers are outside this module's scope.
//! Completed dock, undock, jump and committed ship loss hand off lifecycle events.
//! A committed loss writes the destroyed ship's removal batch to the pilot's
//! connection before the session switches to the escape ship, capsule or home.
//! Initial ballpark state and later updates share the binding's ordered stream.
//! Successful gate and wormhole arrivals record destination jumps and visits in
//! the same transaction as the persisted ship and character locations.

use super::{db_err, Call, CallError, CallResult};
use super::identity::Identity;
use crate::client::Client;
use infinity_core::consts::{dogma::ATTR_GATE_SCRAMBLE_STATUS, FILETIME_SECOND, UE_LOCID};
use crate::db::outbox::Outbox;
use crate::lua::events::GameEvent;
use crate::dao::{characters::ship as character_dao, items::ship as item_dao};
use infinity_core::messages;
use crate::space::{self, event, special_fx, Ball, V3, CLOAK_RESTORE, SPECIAL_FX_UNSPECIFIED_DURATION};
use crate::space::system::{warp::WarpDestination, warp_arrival::{ArrivalCallback, WarpArrivalId}};
use marshal::Value;
use tracing::info;
pub(crate) mod loss;


/// Existing server policy: round to nearest milli-AU/s and floor at 500.
const MIN_SHIP_WARP_FACTOR: i64 = 500;

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


/// Refuse docking, gate jumps and ejecting while the pilot's weapons timer runs (crimewatch).
fn check_weapons_timer(char_id: i64, what: &str) -> Result<(), CallError> {
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


pub(crate) struct SessionChange {
    pub updates: Vec<(&'static str, Value)>,
}

pub(crate) async fn dock<O: observer::DockObserver>(
    identity: &Identity, db: &crate::db::Db, world: &space::World, space_system: Option<i64>,
    station: i64, mut observer: Option<&mut O>,
) -> Result<SessionChange, CallError> {
    let (Some(system), Some(ship)) = (space_system, identity.ship_id) else {
        return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
    };
    check_weapons_timer(identity.char_id, messages::WEAPONS_TIMER_DOCK)?;
    let accepted = world.with(system, move |s| {
        if space::sovereignty::links::blocks(s, ship) {
            return Err(super::notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED));
        }
        if super::effects::active_cycle::mode_locked(s, ship) {
            return Err(super::notify_error(messages::effects::MODE_ACTION_BLOCKED));
        }
        let gap = s.ball(ship).zip(s.ball(station)).map(|(b, station)|
            space::dist(b.position_at(s.now_ms()), station.position_at(s.now_ms()))
                - b.radius as f64 - station.radius as f64);
        if gap.is_none_or(|g| g > DOCKING_RANGE) {
            return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
        }
        s.command(ship, |b, t, _| {
            b.stop();
            vec![event(t, "Stop", vec![Value::Int(b.id)])]
        });
        s.uncloak(ship);
        s.invalidate_missile_target(ship);
        Ok(())
    }).await.ok_or_else(|| CallError::User {
        msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![],
    })?;
    accepted?;
    if let Some(observer) = observer.as_mut() {
        observer.accepted(station);
        if !observer.wait_pumping(system, DOCK_DELAY_MS).await? {
            return Err(CallError::Internal(anyhow::anyhow!("system actor stopped during docking wait")));
        }
    }
    let destination = transport::station::resolve(world.sde().as_ref(), station)?;
    if destination.system_id != system {
        return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
    }
    // Refuse new loaded use on the departing hull and wait out admitted finite
    // work before the market guard serializes the boundary transaction.
    let held_loaded = space::system::boarding::quiesce_ship_loaded(world, system, &[ship]).await
        .map_err(|refusal| refusal.into_call_error(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] }))?;
    let admission = world.admission(system).map_err(db_err)?;
    let market = super::market::lock().await;
    admission.check().map_err(db_err)?;
    let gate = world.lock_condition_snapshot(system).await.map_err(db_err)?;
    let char_id = identity.char_id;
    let transition = world.with(system, move |s| {
        if space::sovereignty::links::blocks(s, ship) {
            return Err(super::notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED));
        }
        if char_id != 0 && s.pilot_ball(char_id).is_none_or(|ball| ball.id != ship) {
            return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
        }
        if super::effects::active_cycle::mode_locked(s, ship) {
            return Err(super::notify_error(messages::effects::MODE_ACTION_BLOCKED));
        }
        let mut transition = s.begin_condition_transitions(&[ship]).ok_or_else(|| CallError::User {
            msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![],
        })?;
        transition.held_loaded = Some(held_loaded);
        Ok(transition)
    }).await.ok_or_else(|| CallError::User {
        msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![],
    })??;
    world.checkpoint_dock(db.clone(), char_id, ship, system, destination, transition, market, gate, admission).await?;
    if let Some(observer) = observer {
        observer.unobserve();
    }
    Ok(SessionChange {
        updates: vec![("solarsystemid", Value::None), ("stationid", Value::Int(station))],
    })
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

async fn publish_ship(world: &space::World, system: i64, ship: balls::PreparedShip,
    market: &crate::services::market::economy_lock::Guard) -> Result<(), CallError> {
    world.system_locked(system, market).await.map_err(CallError::Internal)?;
    let installed = world.with(system, move |s| {
        if s.contains(ship.ball.id) { return false; }
        ship.install(s, |s, ball| s.add_ship(ball));
        true
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    if !installed { return Err(CallError::Internal(anyhow::anyhow!("ship already present in system"))); }
    Ok(())
}

/// Checkpoint and transfer the same live hull while the caller holds market.
async fn transfer_ship(c: &mut Client, system: i64, dest: i64, ship: i64, char_id: i64,
    pos: V3, ancestry: (i64, i64), cloaked: bool, market: crate::services::market::economy_lock::Guard,
    held_loaded: space::system::boarding::HeldLoaded,
    source: crate::space::containment::ActorAdmission, destination: Option<crate::space::containment::ActorAdmission>) -> Result<Option<i64>, CallError> {
    destination.as_ref().map(|admission| admission.check()).transpose().map_err(db_err)?;
    c.world.system_locked(dest, &market).await.map_err(CallError::Internal)?;
    if cloaked {
        if let Err(position) = c.world.request_entry(dest, char_id).await {
            info!(user = c.login.user_id, "jump into {dest} queued at {position}");
            let msg = if position == 0 { "SystemCheck_JumpFailed_Loading" } else { "SystemCheck_JumpFailed_Queued" };
            return Err(CallError::User {
                msg: msg.into(),
                dict: vec![("system".into(), Value::tuple([Value::Int(UE_LOCID), Value::Int(dest)])), ("position".into(), Value::Int(position.max(1)))],
            });
        }
    }
    let first = system.min(dest);
    let second = system.max(dest);
    let mut gates = vec![c.world.lock_condition_snapshot(first).await.map_err(db_err)?];
    if second != first { gates.push(c.world.lock_condition_snapshot(second).await.map_err(db_err)?); }
    let (transition, ball) = c.world.with(system, move |s| {
        if cloaked && space::sovereignty::links::blocks(s, ship) {
            return Err(super::notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED));
        }
        // Ordinary cloaked transfers recheck the mode lock immediately
        // before the condition transition; GM teleport never reaches here.
        if cloaked && super::effects::active_cycle::mode_locked(s, ship) {
            return Err(super::notify_error(messages::effects::MODE_ACTION_BLOCKED));
        }
        let ball = s.pilot_ball(char_id).filter(|ball| ball.id == ship)
            .ok_or_else(|| super::notify_error(messages::MUST_BE_IN_SPACE))?.clone();
        let mut transition = s.begin_condition_transitions(&[ship])
            .ok_or_else(|| super::notify_error(messages::MUST_BE_IN_SPACE))?;
        transition.held_loaded = Some(held_loaded);
        Ok((transition, ball))
    }).await.ok_or_else(|| super::notify_error(messages::MUST_BE_IN_SPACE))??;
    let row = crate::dao::items::space_ships::FreeShip {
        item_id: ship, type_id: ball.type_id, owner_id: ball.owner_id,
        corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos,
    };
    let security = super::crimewatch::security_status(&c.db, char_id).await?;
    let mut prepared = balls::piloted(&c.db, c.sde.clone(), &row, char_id, security).await?;
    let cloak_until = cloaked.then(|| crate::client::filetime_now() + JUMP_CLOAK_SECONDS * FILETIME_SECOND);
    if let Some(until) = cloak_until {
        prepared.ball.cloak = CLOAK_RESTORE;
        prepared.ball.cloak_until = until;
    }
    c.world.checkpoint_transfer(c.db.clone(), char_id, ship, system, dest, pos, ancestry,
        prepared, cloak_until, transition, market, gates, (source, destination)).await?;
    Ok(cloak_until)
}

/// Install an arriving ship with the actor-managed restoring cloak.
pub(super) async fn arrive_cloaked(world: &crate::space::World, system: i64, mut ship: balls::PreparedShip,
    market: &crate::services::market::economy_lock::Guard) -> Result<i64, CallError> {
    let cloak_until = crate::client::filetime_now() + JUMP_CLOAK_SECONDS * FILETIME_SECOND;
    ship.ball.cloak = CLOAK_RESTORE;
    ship.ball.cloak_until = cloak_until;
    publish_ship(world, system, ship, market).await?;
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
    check_weapons_timer(char_id, messages::WEAPONS_TIMER_JUMP)?;
    let dest = gate_destination(&c.sde, from_gate, to_gate, system)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_JUMP_GATE_HAS_NO_DESTINATION.into(), dict: vec![] })?;
    let dest_system: i64 = dest.dest_solar_system_id;

    // In range of the gate, and not gate-scrambled? One actor turn reads both:
    // the distance scalar first, then the ship's resident fit, so a missing
    // combat record refuses nothing extra.
    let (gap, gate_scrambled) = c.world.with(system, move |s| {
        let (b, gate) = (s.ball(ship)?, s.ball(from_gate)?);
        let gap = space::dist(b.position_at(s.now_ms()), gate.position_at(s.now_ms()))
            - gate.radius as f64
            - b.radius as f64;
        let scrambled = s.with_combat_fit(ship, |fit, _, _|
            fit.value(super::fit::Key::Ship, ATTR_GATE_SCRAMBLE_STATUS) > 0.0)
            .unwrap_or(false);
        Some((gap, scrambled))
    }).await.flatten().unwrap_or((f64::MAX, false));
    if gap > JUMP_RANGE {
        return Err(CallError::User { msg: messages::MSG_NOT_CLOSE_ENOUGH_TO_JUMP.into(), dict: vec![("maxdist".into(), Value::Int(JUMP_RANGE as i64))] });
    }
    if gate_scrambled {
        return Err(CallError::User { msg: messages::MSG_GATE_SCRAMBLED.into(), dict: vec![] });
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

    // Jump out where everyone can see it, and let the gate animation run.
    c.world.with(system, move |s| {
        if space::sovereignty::links::blocks(s, ship) {
            return Err(super::notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED));
        }
        if s.super_weapon_jump_cloak_until(ship).is_some_and(|until| until > s.now_ms()) {
            return Err(super::notify_error(messages::super_weapon::JUMP_LOCKED));
        }
        if super::effects::active_cycle::mode_locked(s, ship) {
            return Err(super::notify_error(messages::effects::MODE_ACTION_BLOCKED));
        }
        s.uncloak(ship);
        s.command(ship, move |b, t, _| {
            b.stop();
            vec![
                event(t, "Stop", vec![Value::Int(b.id)]),
                special_fx(t, b.id, None, Some(through), other_type, guid, false, 1, 0,
                    Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0,
                    Value::tuple([Value::Int(dest_system)])),
            ]
        });
        s.invalidate_missile_target(ship);
        Ok(())
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("source system actor stopped")))??;
    if !c.wait_pumping(system, JUMP_DELAY_MS).await.map_err(CallError::Internal)? {
        return Err(CallError::Internal(anyhow::anyhow!("source system actor stopped during jump wait")));
    }
    let source = c.world.admission(system).map_err(db_err)?;
    let destination = (dest_system != system).then(|| c.world.admission(dest_system)).transpose().map_err(db_err)?;
    // Refuse new loaded use on the departing hull and wait out admitted finite
    // work before the market guard serializes the boundary transaction.
    let held_loaded = space::system::boarding::quiesce_ship_loaded(&c.world, system, &[ship]).await
        .map_err(|refusal| refusal.into_call_error(super::notify_error(messages::fitting::CHARGES_SETTLING)))?;
    let market = super::market::lock().await;
    source.check().map_err(db_err)?;
    let cloak_until = transfer_ship(c, system, dest_system, ship, char_id, pos, (constellation, region), true, market, held_loaded, source, destination).await?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("jump cloak missing")))?;
    c.leave_space();
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
        c.world.post(dest_system, move |s| {
            let Some(token) = s.capture_ball(ship) else { return };
            s.schedule(WORMHOLE_EFFECT_STOP_DELAY_MS, move |s| {
                if s.ball_token_current(token) {
                    s.publish_destiny(ship, |stamp| vec![special_fx(stamp, ship, None, None, None,
                        "effects.CloakRegardless", false, 0, 0, Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION),
                        0, None, 0, Value::None)]);
                }
            });
        });
    }
    Ok(())
}


/// GM teleport (/tr): put the ship at `pos` in `dest_system` at once. Within the system
/// the client gets a fresh ballpark; elsewhere the session moves as on a jump.
pub(super) async fn teleport(c: &mut Client, dest_system: i64, pos: V3) -> Result<(), CallError> {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::MUST_BE_IN_SPACE))] });
    };
    let source = c.world.admission(system).map_err(db_err)?;
    let destination = (dest_system != system).then(|| c.world.admission(dest_system)).transpose().map_err(db_err)?;
    // Refuse new loaded use on the departing hull and wait out admitted finite
    // work before the market guard serializes the boundary transaction.
    let held_loaded = space::system::boarding::quiesce_ship_loaded(&c.world, system, &[ship]).await
        .map_err(|refusal| refusal.into_call_error(super::notify_error(messages::fitting::CHARGES_SETTLING)))?;
    let market = super::market::lock().await;
    source.check().map_err(db_err)?;
    let (constellation, region) = ancestry(&c.sde, dest_system)?;
    transfer_ship(c, system, dest_system, ship, char_id, pos, (constellation, region), false, market, held_loaded, source, destination).await?;
    if dest_system == system {
        return send_state(c).await;
    }
    c.leave_space();
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

/// Apply a committed ship loss to the connected pilot's session.
pub async fn lose_ship(c: &mut Client, ship: i64, replacement_ship: i64, home: Option<(i64, i64, i64, i64)>, _pos: [f64; 3]) -> Result<(), CallError> {
    let (Some(char_id), Some(system)) = (c.session.int("charid"), c.session.int("solarsystemid")) else { return Ok(()) };
    if c.session.int("shipid") != Some(ship) { return Ok(()); }
    // The committed loss's destroy/removal batch must reach the pilot's connection
    // before the session switch to the replacement ship or the home station.
    if home.is_some() {
        c.notify("OnPodDeath", vec![]);
        c.notify("OnPlayerPodDeath", vec![]);
    }
    c.flush_destiny_actions(system).await.map_err(CallError::Internal)?;
    if let Some((station, system, constellation, region)) = home {
        c.notify("OnPodKilled", vec![Value::Int(ship)]);
        c.leave_space();
        c.change_session(vec![
            ("solarsystemid", Value::None), ("stationid", Value::Int(station)),
            ("solarsystemid2", Value::Int(system)), ("constellationid", Value::Int(constellation)),
            ("regionid", Value::Int(region)), ("shipid", Value::Int(replacement_ship)),
        ]);
        Ok(())
    } else {
        info!(user = c.login.user_id, "ship {ship} lost; {char_id} moved to ship {replacement_ship}");
        c.change_session(vec![("shipid", Value::Int(replacement_ship))]);
        send_state(c).await
    }
}

/// Push the full destiny state for the client's current system (on beyonce bind / UpdateStateRequest).
pub async fn send_state(c: &mut Client) -> Result<(), CallError> {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(()) };
    c.reset_destiny();
    c.world.observe(system, c.login.client_id, ship, c.destiny_tx.clone()).await.map_err(CallError::Internal)?;
    c.drain_destiny();
    Ok(())
}

/// Steer or stop the ship, dropping a jump cloak.
fn movement(identity: &Identity, world: &space::World, space_system: Option<i64>, f: impl FnOnce(&mut Ball, i64, &space::System) -> Vec<Value> + Send + 'static) -> CallResult {
    let (Some(system), Some(ship)) = (space_system, identity.ship_id) else { return Ok(Value::None) };
    world.post(system, move |s| {
        // A superweapon's immobility deadline refuses steering outright; the
        // arrival jump cloak stays since no command was accepted.
        if s.super_weapon_immobile_until(ship).is_some_and(|until| until > s.now_ms()) { return; }
        // An admitted ship mode refuses steering at the posted admission and
        // again inside the queued command: the mode may start between the
        // enqueue and the action application, where the ball is temporarily
        // removed, so the check reads the resident record by ship ID.
        if super::effects::active_cycle::mode_locked(s, ship) { return; }
        s.drop_jump_cloak(ship);
        s.command(ship, move |ball, stamp, s| {
            if super::effects::active_cycle::mode_locked(s, ship) { return Vec::new(); }
            f(ball, stamp, s)
        });
    });
    Ok(Value::None)
}

pub(crate) async fn warp_to(identity: &Identity, db: &crate::db::Db, world: &space::World, space_system: Option<i64>,
    destination: WarpDestination, on_arrival: Option<ArrivalCallback>) -> CallResult {
    if !destination.is_valid() { return Ok(Value::None); }
    let (Some(system), Some(ship)) = (space_system, identity.ship_id) else { return Ok(Value::None) };
    let guard = match on_arrival {
        Some(callback) => match world.begin_warp_arrival(system, callback).await {
            Some(guard) => Some(guard), None => return Ok(Value::None),
        },
        None => None,
    };
    warp_pilot(db, world, system, identity.char_id, ship, destination, guard.as_ref().map(|g| g.id()), None).await?;
    Ok(Value::None)
}

pub(super) async fn warp_destination(identity: &Identity, db: &crate::db::Db, world: &space::World, space_system: Option<i64>,
    order: Option<super::fleets::warp::Order>, destination: WarpDestination, on_arrival: Option<ArrivalCallback>, fleet_destination: Value) -> CallResult {
    match order {
        Some(order) => super::fleets::warp::execute(db, world, space_system, order, destination, on_arrival, fleet_destination).await,
        None => warp_to(identity, db, world, space_system, destination, on_arrival).await,
    }
}

fn fitted_warp_factor(fit: &super::fit::Fit) -> Result<i64, CallError> {
    use infinity_core::consts::dogma::{ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER};
    let base = fit.value(super::fit::Key::Ship, ATTR_BASE_WARP_SPEED);
    let multiplier = fit.value(super::fit::Key::Ship, ATTR_WARP_SPEED_MULTIPLIER);
    if !base.is_finite() || base <= 0.0 || !multiplier.is_finite() || multiplier <= 0.0 {
        return Err(CallError::Internal(anyhow::anyhow!("invalid fitted warp speed attributes")));
    }
    Ball::warp_factor_from_speed(base * multiplier)
        .map(|factor| factor.max(MIN_SHIP_WARP_FACTOR))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("invalid fitted warp speed")))
}

/// Load and fence each pilot's own fit, retrying stale projection snapshots.
pub(super) async fn warp_pilot(db: &crate::db::Db, world: &space::World, system: i64, pilot: i64, ship: i64,
    destination: WarpDestination, arrival: Option<WarpArrivalId>,
    order: Option<std::sync::Arc<super::fleets::warp::Order>>) -> Result<bool, CallError> {
    loop {
        let fit = super::fit::Fit::load_in_space(db, world, system, pilot, ship).await?;
        let Some(revision) = fit.projection_revision() else { return Ok(false) };
        let scrambled = fit.value(super::fit::Key::Ship, infinity_core::consts::dogma::ATTR_WARP_SCRAMBLE_STATUS) > 0.0;
        let factor = fitted_warp_factor(&fit)?;
        let is_fleet = order.is_some();
        let guard = order.clone();
        let result = world.with(system, move |s| {
            if let Some(order) = guard {
                super::fleets::warp::commit(s, &order, pilot, ship, |s| commit_warp(s, ship, revision, scrambled, factor, destination, arrival))
            } else {
                Some(commit_warp(s, ship, revision, scrambled, factor, destination, arrival))
            }
        }).await.flatten().unwrap_or(WarpCommit::Unavailable);
        match result {
            WarpCommit::Queued => return Ok(true),
            WarpCommit::RetryProjection => continue,
            WarpCommit::Unavailable => return Ok(false),
            WarpCommit::EntosisBlocked if !is_fleet => return Err(super::notify_error(messages::effects::ENTOSIS_ACTION_BLOCKED)),
            WarpCommit::ModeBlocked if !is_fleet => return Err(super::notify_error(messages::effects::MODE_ACTION_BLOCKED)),
            WarpCommit::Scrambled if !is_fleet => return Err(super::notify_error(messages::effects::WARP_SCRAMBLED)),
            WarpCommit::Immobile if !is_fleet => return Err(super::notify_error(messages::super_weapon::WARP_LOCKED)),
            WarpCommit::EntosisBlocked | WarpCommit::ModeBlocked | WarpCommit::Scrambled | WarpCommit::Immobile => return Ok(false),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WarpCommit { Queued, RetryProjection, Scrambled, Immobile, EntosisBlocked, ModeBlocked, Unavailable }

pub(super) fn commit_warp(s: &mut space::System, ship: i64, revision: u64, scrambled: bool,
    factor: i64, destination: WarpDestination, arrival: Option<WarpArrivalId>) -> WarpCommit {
    if super::effects::active_cycle::mode_locked(s, ship) { return WarpCommit::ModeBlocked; }
    if space::sovereignty::links::blocks(s, ship) { return WarpCommit::EntosisBlocked; }
    if s.super_weapon_immobile_until(ship).is_some_and(|until| until > s.now_ms()) { return WarpCommit::Immobile; }
    if s.projection_revision(ship) != revision { return WarpCommit::RetryProjection; }
    if scrambled || s.interdicted(ship) { return WarpCommit::Scrambled; }
    s.drop_jump_cloak(ship);
    if s.queue_warp(ship, factor, destination, arrival) { WarpCommit::Queued } else { WarpCommit::Unavailable }
}

/// Ensure a stationary ship starts moving when given a heading (client sends 1.0 separately too).
fn ensure_moving(b: &mut Ball, tick: i64, ev: &mut Vec<Value>) {
    if b.speed_fraction <= 0.0 {
        b.set_speed_fraction(1.0);
        ev.push(event(tick, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]));
    }
}

pub(crate) mod appearance;
mod ship_kill_counter;
pub(crate) mod balls;
pub(in crate::services) mod boarding;
mod configuration;
mod cleaning;
mod stored;
mod eject;
pub(crate) mod observer;
pub(crate) mod ship;
mod deployment;
mod undock_point;
pub(crate) mod beyonce;
pub(crate) mod transport;
mod navigation;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "ship" => ship::dispatch(c, call).await,
        "beyonce" => beyonce::dispatch(c, call).await,
        "shipKillCounter" => ship_kill_counter::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(all(test, feature = "tests-ships"))] mod tests;
