//! jumpCloneSvc station-bound calls and clone transitions.
//!
//! Mirrors eve/client/script/ui/services/clonejumpsvc.py, using game clone and
//! item data, skill levels, and the startup SDE. Implants and ship-borne clone
//! vat bays are not modelled.

use super::super::{crowset, db_err, notify_error, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{FILETIME_HOUR, FLAG_HANGAR, TYPE_CAPSULE, SKILL_INFOMORPH_PSYCHOLOGY, SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY};
use crate::dao::{characters::clones as clone_dao, items::{clones as item_dao, creation}, skills::levels as skill_dao};
use crate::db::outbox::{Effect, Outbox};
use crate::messages;
use marshal::{DbRowDescriptor, DbType, Value};
use tracing::info;

use super::policy::jump_cooldown;

const SKILL_INFOMORPH_SYNCHRONIZING: i32 = 33399;
/// Existing server policy: maximum jump clone name characters.
const MAX_CLONE_NAME_LENGTH: usize = 100;


enum JumpEffect {
    Trade(Outbox<crate::services::trade::TradeEffect>),
    Session { station: i64, system: i64, constellation: i64, region: i64, capsule: i64 },
}

/// The character's clones as `(jumpCloneID, ownerID, locationID, typeID, cloneName)` rows.
async fn clone_rows(c: &Client, me: i64) -> Result<Value, CallError> {
    let header = DbRowDescriptor::new([
        ("jumpCloneID", DbType::I4),
        ("ownerID", DbType::I4),
        ("locationID", DbType::I8),
        ("typeID", DbType::I4),
        ("cloneName", DbType::WStr),
    ]);
    let rows = clone_dao::clones(&c.db, me).await.map_err(db_err)?;
    let lines = rows
        .iter()
        .map(|r| header.row(&[Value::Int(r.clone_id), Value::Int(me), Value::Int(r.location_id), Value::Int(TYPE_CAPSULE as i64), Value::unicode(&r.name)]))
        .collect();
    Ok(crowset(&header, lines))
}

/// `GetCloneState` -> KeyVal(clones, implants, timeLastJump).
async fn clone_state(c: &Client, me: i64) -> CallResult {
    let last = clone_dao::last_jump(&c.db, me).await.map_err(db_err)?;
    Ok(super::super::keyval([
        ("clones", clone_rows(c, me).await?),
        ("implants", crowset(&DbRowDescriptor::new([("jumpCloneID", DbType::I4), ("typeID", DbType::I4)]), vec![])),
        ("timeLastJump", last.map(Value::Int).unwrap_or(Value::None)),
    ]))
}

async fn install(c: &Client, me: i64) -> CallResult {
    let Some(station) = c.session.int("stationid") else {
        return Err(notify_error(messages::STATION_DOCK_REQUIRED_FOR_JUMP_CLONE_INSTALL));
    };
    let max = skill_dao::level(&c.db, me, SKILL_INFOMORPH_PSYCHOLOGY).await.map_err(db_err)?.unwrap_or(0) as i64
        + skill_dao::level(&c.db, me, SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY).await.map_err(db_err)?.unwrap_or(0) as i64;
    let rows = clone_dao::locations(&c.db, me).await.map_err(db_err)?;
    if rows.contains(&station) {
        return Err(notify_error(messages::JUMP_CLONE_ALREADY_AT_STATION));
    }
    if rows.len() as i64 >= max {
        return Err(notify_error(&messages::jump_clone_limit(max)));
    }
    let inserted = clone_dao::install(&c.db, me, station).await.map_err(db_err)?;
    if inserted == 0 {
        return Err(notify_error(messages::JUMP_CLONE_ALREADY_AT_STATION));
    }
    info!("clones: {me} installed a jump clone at {station}");
    c.world.notify_pilot(me, "OnJumpCloneCacheInvalidated", vec![]);
    Ok(Value::None)
}

/// `CloneJump(destLocationID, cloneID, cost, confirmed)`.
async fn jump(c: &mut Client, me: i64, dest: i64, clone_id: i64) -> CallResult {
    let Some(station) = c.session.int("stationid") else {
        return Err(CallError::User { msg: messages::MSG_NOT_AT_STATION.into(), dict: vec![] });
    };
    let now = filetime_now();
    let ship = c.session.int("shipid");
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        |tx, outbox: &mut Outbox<JumpEffect>| {
            let sde = sde.clone();
            Box::pin(async move {
            let last = clone_dao::last_jump(tx, me).await.map_err(db_err)?;
            let level = skill_dao::level(tx, me, SKILL_INFOMORPH_SYNCHRONIZING).await.map_err(db_err)?.unwrap_or(0) as i64;
            let policy = &crate::config::game().clones;
            let cooldown = jump_cooldown(policy.jump_cooldown_hours, policy.synchronizing_hours_per_level, level);
            if let Some(ready) = last.map(|l| i128::from(l) + cooldown).filter(|r| *r > i128::from(now)) {
                let hours = (ready - i128::from(now)) as f64 / FILETIME_HOUR as f64;
                return Err(notify_error(&messages::jump_clone_cooldown(hours)));
            }
            let target = clone_dao::target(tx, clone_id, me, dest).await.map_err(db_err)?
                .and_then(|id| sde.map.station(id)).ok_or_else(|| notify_error(messages::JUMP_CLONE_NOT_FOUND))?;
            let required = |v: Option<i64>| v.ok_or_else(|| CallError::Internal(anyhow::anyhow!("jump clone station required field is null")));
            let (to, system, constellation, region) = (target.station_id, required(target.solar_system_id)?, required(target.constellation_id)?, required(target.region_id)?);
            if to == station {
                return Err(notify_error(messages::ALREADY_AT_JUMP_CLONE_DESTINATION));
            }
            let mut trade_outbox = Outbox::default();
            crate::services::trade::abort_for_char(tx, &sde, &mut trade_outbox, me).await?;
            outbox.operation(JumpEffect::Trade(trade_outbox));
            clone_dao::delete(tx, clone_id).await.map_err(db_err)?;
            clone_dao::install(tx, me, station).await.map_err(db_err)?;
            if let Some(ship) = ship {
                item_dao::delete_capsule(tx, ship, TYPE_CAPSULE).await.map_err(db_err)?;
            }
            let capsule = creation::rookie_ship(tx, TYPE_CAPSULE, me, to, FLAG_HANGAR).await.map_err(db_err)?;
            clone_dao::jump(tx, me, capsule, to, system, constellation, region, now).await.map_err(db_err)?;
            outbox.operation(JumpEffect::Session { station: to, system, constellation, region, capsule });
            outbox.notify_pilot(me, "OnJumpCloneCacheInvalidated", vec![]);
            Ok(to)
            })
        },
        |to, outbox| async move {
            let world = c.world.clone();
            for effect in outbox.into_entries() {
                match effect {
                    Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *c))?,
                    Effect::Operation(JumpEffect::Trade(trade_outbox)) => crate::services::trade::dispatch((), trade_outbox, &world)?,
                    Effect::Event(event) => crate::lua::on_event(event),
                    Effect::Operation(JumpEffect::Session { station, system, constellation, region, capsule }) => c.change_session(vec![
                        ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system)),
                        ("constellationid", Value::Int(constellation)), ("regionid", Value::Int(region)),
                        ("shipid", Value::Int(capsule)),
                    ]),
                }
            }
            info!("clones: {me} clone jumped from {station} to {to}");
            Ok(Value::None)
        },
    ).await
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "jumpCloneSvc" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "GetCloneState" => clone_state(c, me).await,
        "GetShipCloneState" | "GetStationCloneState" => {
            let header = DbRowDescriptor::new([("jumpCloneID", DbType::I4), ("ownerID", DbType::I4), ("locationID", DbType::I8), ("typeID", DbType::I4), ("cloneName", DbType::WStr)]);
            Ok(crowset(&header, vec![]))
        }
        "GetNumClonesInPilotsStructure" => Ok(Value::Int(0)),
        "ValidateInstallJumpClone" => Ok(Value::list([])),
        "GetPriceForClone" => Ok(Value::Float(0.0)),
        "InstallCloneInStation" => install(c, me).await,
        "InstallCloneInStructure" => Err(notify_error(messages::CLONE_STRUCTURE_INSTALL_UNSUPPORTED)),
        "DestroyInstalledClone" => match clone_dao::destroy(&c.db, int(0), me).await {
            Ok(_) => {
                c.world.notify_pilot(me, "OnJumpCloneCacheInvalidated", vec![]);
                Ok(Value::None)
            }
            Err(e) => Err(db_err(e)),
        },
        // SetJumpCloneName(cloneID, name)
        "SetJumpCloneName" => {
            let name = value_text(call.args.get(1), MAX_CLONE_NAME_LENGTH);
            clone_dao::rename(&c.db, int(0), me, &name).await.map(|_| Value::None).map_err(db_err)
        }
        "CloneJump" => jump(c, me, int(0), int(1)).await,
        "ResetLastCloneJumpTime" => Ok(Value::None),
        _ => return None,
    })
}
