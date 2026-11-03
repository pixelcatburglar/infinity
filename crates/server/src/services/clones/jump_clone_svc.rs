//! Station-bound jumpCloneSvc RPC adapter, mirroring clonejumpsvc.py.
//! Reads game.jump_clones and item custody through the clone DAOs; body mutation
//! lives in the lifecycle and transition modules. Ship clone bays are unsupported.

use super::{lifecycle, policy, transition, CloneEffect, MAX_CLONE_NAME_LENGTH};
use super::super::{crowset, db_err, notify_error, Call, CallError, CallResult};
use crate::{client::{filetime_now, Client}, consts::{GM_ROLES, ROLE_QA, TYPE_CAPSULE},
    dao::{characters::clones as clone_dao, items::clones as item_dao},
    db::outbox::{Effect, Outbox}, messages, services::{effects::online, identity::Identity, market, trade}};
use marshal::{DbRowDescriptor, DbType, Value};

fn positive_id(value: &Value) -> Option<i64> {
    match value { Value::Int(id) if *id > 0 => Some(*id), _ => None }
}

fn jump_arguments(args: &[Value]) -> Option<(i64, i64, f64, bool)> {
    let [destination, clone, quote, Value::Bool(confirmed)] = args else { return None };
    let quote = match quote { Value::Int(v) => *v as f64, Value::Float(v) => *v, _ => return None };
    if !quote.is_finite() || quote < 0.0 { return None; }
    Some((positive_id(destination)?, positive_id(clone)?, quote, *confirmed))
}

fn clone_name_arguments(args: &[Value]) -> Option<(i64, &str)> {
    let [id, Value::Unicode(name)] = args else { return None };
    if name.chars().count() > MAX_CLONE_NAME_LENGTH || name.contains('\0') { return None; }
    Some((positive_id(id)?, name))
}

fn invalid() -> CallError { notify_error(messages::clones::INVALID_ARGUMENTS) }

fn clone_rows(rows: &[clone_dao::CloneRow]) -> Value {
    let header = DbRowDescriptor::new([
        ("jumpCloneID", DbType::I8), ("ownerID", DbType::I4),
        ("locationID", DbType::I8), ("typeID", DbType::I4), ("cloneName", DbType::WStr),
    ]);
    let lines = rows.iter().map(|r| header.row(&[
        Value::Int(r.clone_id), Value::Int(r.owner_id), Value::Int(r.location_id),
        Value::Int(TYPE_CAPSULE as i64), Value::unicode(&r.name),
    ])).collect();
    crowset(&header, lines)
}

async fn clone_state(c: &Client, me: i64, station_only: bool) -> CallResult {
    c.db.transaction(
        |tx, _: &mut Outbox| Box::pin(async move {
            let character = clone_dao::lock_character(tx, me).await.map_err(db_err)?
                .ok_or_else(|| notify_error(messages::JUMP_CLONE_NOT_FOUND))?;
            let mut rows = clone_dao::clones(tx, me).await.map_err(db_err)?;
            if station_only {
                rows.retain(|r| Some(r.location_id) == character.station_id);
                return Ok(clone_rows(&rows));
            }
            let header = DbRowDescriptor::new([("jumpCloneID", DbType::I8), ("typeID", DbType::I4)]);
            let mut implants = Vec::new();
            for row in &rows {
                for item in item_dao::list(tx, me, row.clone_id).await.map_err(db_err)? {
                    implants.push(header.row(&[Value::Int(row.clone_id), Value::Int(i64::from(item.type_id))]));
                }
            }
            Ok(super::super::keyval([
                ("clones", clone_rows(&rows)),
                ("implants", crowset(&header, implants)),
                ("timeLastJump", character.last_clone_jump.map(Value::Int).unwrap_or(Value::None)),
            ]))
        }),
        |value, _: Outbox| async move { Ok(value) },
    ).await
}

fn dispatch_effects(c: &mut Client, outbox: Outbox<CloneEffect>) -> Result<(), CallError> {
    let world = c.world.clone();
    for entry in outbox.into_entries() {
        match entry {
            Effect::Notification(notification) => online::dispatch_notification(notification, &world, &c.notify_tx, c.session.int("charid"))?,
            Effect::Operation(CloneEffect::Online(change)) => online::apply_online_changed(change),
            Effect::Operation(CloneEffect::Trade(effects)) => trade::dispatch((), effects, &world)?,
            Effect::Event(event) => crate::lua::on_event(event),
        }
    }
    Ok(())
}

#[derive(Clone)]
enum Mutation {
    Install,
    Destroy(i64),
    Rename(i64, String),
    Reset,
    Jump(i64, i64, f64, bool),
}

async fn mutate(c: &mut Client, mutation: Mutation) -> CallResult {
    let id = Identity::of_client(c);
    if id.char_id <= 0 { return Err(invalid()); }
    let now = filetime_now();
    let _guard = market::lock().await;
    let char_id = id.char_id;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        |tx, outbox: &mut Outbox<CloneEffect>| {
            let sde = sde.clone();
            let id = Identity { char_id: id.char_id, ship_id: id.ship_id, corp_id: id.corp_id, station_id: id.station_id, system_id: id.system_id };
            let mutation = mutation.clone();
            Box::pin(async move {
                match mutation {
                    Mutation::Install => { lifecycle::install_tx(tx, outbox, &sde, &id, now).await?; Ok(None) }
                    Mutation::Destroy(clone) => { lifecycle::destroy_tx(tx, outbox, &id, clone).await?; Ok(None) }
                    Mutation::Rename(clone, name) => { lifecycle::rename_tx(tx, outbox, &id, clone, &name).await?; Ok(None) }
                    Mutation::Reset => { lifecycle::reset_tx(tx, outbox, &id).await?; Ok(None) }
                    Mutation::Jump(dest, clone, quote, confirmed) => transition::jump_tx(tx, outbox, &sde, &id, dest, clone, quote, confirmed, now).await.map(Some),
                }
            })
        },
        |transition, outbox| async move {
            if let Some(t) = transition {
                c.change_session(vec![
                    ("stationid", Value::Int(t.station)), ("solarsystemid2", Value::Int(t.system)),
                    ("constellationid", Value::Int(t.constellation)), ("regionid", Value::Int(t.region)),
                    ("shipid", Value::Int(t.capsule)),
                ]);
                c.world.notify_pilot(char_id, "OnJumpCloneTransitionCompleted", vec![]);
            }
            let result = dispatch_effects(c, outbox);
            crate::scheduler::wake();
            result?;
            Ok(Value::None)
        },
    ).await
}

async fn validate(c: &Client) -> CallResult {
    let id = Identity::of_client(c);
    if id.char_id <= 0 { return Err(invalid()); }
    let now = filetime_now();
    let sde = c.sde.clone();
    c.db.transaction(
        |tx, _: &mut Outbox| {
            let sde = sde.clone();
            let id = Identity { char_id: id.char_id, ship_id: id.ship_id, corp_id: id.corp_id, station_id: id.station_id, system_id: id.system_id };
            Box::pin(async move { policy::validate_install_tx(tx, &sde, &id, now).await.map(Value::list) })
        },
        |value, _: Outbox| async move { Ok(value) },
    ).await
}
fn ship_clone_state(args: &[Value]) -> CallResult {
    if !args.is_empty() { return Err(invalid()); }
    Ok(clone_rows(&[]))
}


pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "jumpCloneSvc" { return None; }
    let me = c.session.int("charid").unwrap_or(0);
    let no_args = || if call.args.is_empty() { Ok(()) } else { Err(invalid()) };
    Some(match call.method {
        "GetCloneState" => match no_args() { Ok(()) if me > 0 => clone_state(c, me, false).await, _ => Err(invalid()) },
        "GetStationCloneState" => match no_args() { Ok(()) if me > 0 => clone_state(c, me, true).await, _ => Err(invalid()) },
        "GetShipCloneState" => ship_clone_state(call.args),
        "GetNumClonesInPilotsStructure" => Ok(Value::Int(0)),
        "ValidateInstallJumpClone" => match no_args() { Ok(()) => validate(c).await, Err(e) => Err(e) },
        "GetPriceForClone" => match no_args() { Ok(()) => Ok(Value::Float(crate::config::game().clones.service_fee_isk)), Err(e) => Err(e) },
        "InstallCloneInStation" => match no_args() { Ok(()) => mutate(c, Mutation::Install).await, Err(e) => Err(e) },
        "InstallCloneInStructure" => Err(notify_error(messages::CLONE_STRUCTURE_INSTALL_UNSUPPORTED)),
        "DestroyInstalledClone" => match call.args { [value] => match positive_id(value) { Some(id) => mutate(c, Mutation::Destroy(id)).await, None => Err(invalid()) }, _ => Err(invalid()) },
        "SetJumpCloneName" => match clone_name_arguments(call.args) { Some((id, name)) => mutate(c, Mutation::Rename(id, name.to_owned())).await, None => Err(invalid()) },
        "CloneJump" => match jump_arguments(call.args) { Some((dest, clone, quote, confirmed)) => mutate(c, Mutation::Jump(dest, clone, quote, confirmed)).await, None => Err(invalid()) },
        "ResetLastCloneJumpTime" => match no_args() {
            Err(e) => Err(e),
            Ok(()) if c.login.role & (GM_ROLES | ROLE_QA) == 0 => Err(notify_error(messages::clones::RESET_ROLE_REQUIRED)),
            Ok(()) => mutate(c, Mutation::Reset).await,
        },
        _ => return None,
    })
}

#[cfg(test)] mod tests;
