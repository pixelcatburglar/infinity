//! Shared expert-system lifecycle and timeline integration.
//!
//! Source: expertSystemMgr and the expert-system client rules.
//! State: game expert installations, skill timeline, inventory and scheduled jobs.
//! Notifications are queued transactionally and dispatched only after commit.
//! Unsupported: client-supplied duration or lifecycle overrides.
use std::sync::Arc;
use marshal::Value;
use infinity_core::{consts::FILETIME_DAY, messages};
use crate::{client::filetime_now, dao::{items::loading::ItemQuery, skills::expert_systems as dao}, db::{self, outbox::{Notification, Outbox}}, sde::Sde, services::{db_err, notify_error, effects::online::{self, OnlineChanged}, identity::Identity, inventory::{self, reachability::{Endpoint, Reachability}}, CallError}, space::World};

/// expertSystems/const.py:7, MAX_CHARACTER_INSTALLATIONS.
const MAX_ACTIVE: usize = 3;
/// expertSystems/const.py:8, MAX_INSTALLED_DURATION_TO_ALLOW_TOP_UP.
const RENEWAL_WINDOW: i64 = FILETIME_DAY * 30;
const EVENT: &str = "OnExpertSystemsUpdated";

pub(in crate::services) fn map(rows: &[dao::ExpertSystem]) -> Value {
    Value::Dict(rows.iter().map(|row| (Value::Int(i64::from(row.type_id)), Value::Tuple(vec![Value::Int(row.installed_at), Value::Int(row.expires_at)]))).collect())
}

fn changed<E>(outbox: &mut Outbox<E>, char_id: i64, rows: &[dao::ExpertSystem], added: bool, type_id: i32) {
    outbox.notify_pilot(char_id, EVENT, vec![map(rows), Value::Bool(added), if type_id == 0 { Value::None } else { Value::Int(i64::from(type_id)) }]);
}

/// Advance the same timeline used by skill completion, including expert expiry.
pub(crate) async fn advance_expired<F>(db: &db::Db, world: &World, sde: &Arc<Sde>, char_id: i64, at: i64, mut dispatch: F) -> Result<(), CallError>
where F: FnMut(Notification) -> Result<(), CallError> + Send {
    if char_id <= 0 { return Err(notify_error(messages::expert_systems::INVALID)); }
    let _market = crate::services::market::lock().await;
    let sde = Arc::clone(sde);
    let fences = online::capture_online_fences(world, &[char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = Arc::clone(&sde);
        let fences = fences.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            let at = at.max(filetime_now());
            let progress = super::completion::advance_tx(tx, outbox, &sde, char_id, at).await?;
            super::completion::finish_tx(tx, outbox, &sde, &fences, char_id, at, &progress, &[]).await
        })
    }, |value, outbox| async move { let result = online::dispatch_entries(value, outbox, world, &mut dispatch); crate::scheduler::wake(); result }).await
}

pub(crate) async fn current<F>(identity: &Identity, db: &db::Db, world: &World, sde: &Arc<Sde>, dispatch: F) -> Result<Value, CallError>
where F: FnMut(Notification) -> Result<(), CallError> + Send {
    advance_expired(db, world, sde, identity.char_id, filetime_now(), dispatch).await?;
    Ok(map(&dao::active(db, identity.char_id, filetime_now()).await.map_err(db_err)?))
}

#[derive(Clone, Copy)]
enum Action { Consume(i64), Install(i32), Remove(i32) }

async fn mutate<F>(identity: &Identity, db: &db::Db, world: &World, sde: &Arc<Sde>, action: Action, mut dispatch: F) -> Result<(), CallError>
where F: FnMut(Notification) -> Result<(), CallError> + Send {
    if identity.char_id <= 0 { return Err(notify_error(messages::expert_systems::INVALID)); }
    let _market = crate::services::market::lock().await;
    let captured = if let Action::Consume(item_id) = action {
        let item = crate::services::items::load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(|| notify_error(messages::expert_systems::ITEM_NOT_FOUND))?;
        let endpoint = Endpoint { location: item.location_id, flag: item.flag_id };
        Some((Reachability::capture(identity, db, world, identity.system_id, &[endpoint]).await?,
            inventory::notifications::Audience::capture(identity, db, world, identity.system_id, &[endpoint]).await?))
    } else { None };
    let identity_fields = (identity.char_id, identity.ship_id, identity.corp_id, identity.station_id, identity.system_id);
    let sde = Arc::clone(sde);
    let fences = online::capture_online_fences(world, &[identity.char_id]).await;
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let (char_id, ship_id, corp_id, station_id, system_id) = identity_fields;
        let identity = Identity { char_id, ship_id, corp_id, station_id, system_id };
        let sde = Arc::clone(&sde);
        let captured = captured.clone();
        let fences = fences.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            let now = filetime_now();
            let mut progress = super::completion::advance_tx(tx, outbox, &sde, char_id, now).await?;
            let before = super::attributes::character_attributes_at(tx, &sde, char_id, now).await?;
            let rows = dao::lock_rows(tx, char_id).await.map_err(db_err)?;
            let type_id = match action {
                Action::Consume(item_id) => {
                    let (reachability, audience) = captured.as_ref().expect("consume capture");
                    inventory::expert_systems::validate_and_consume(tx, &identity, &sde, reachability, audience, item_id, outbox).await?.type_id
                },
                Action::Install(type_id) | Action::Remove(type_id) => type_id,
            };
            let def = sde.expert_systems.get(type_id).ok_or_else(|| notify_error(messages::expert_systems::INVALID))?;
            let existing = rows.iter().find(|row| row.type_id == type_id);
            let added = existing.is_none();
            match action {
                Action::Remove(_) => {
                    let row = existing.ok_or_else(|| notify_error(messages::expert_systems::NOT_INSTALLED))?;
                    if !dao::delete(tx, char_id, type_id, row.expires_at).await.map_err(db_err)? { return Err(notify_error(messages::expert_systems::CHANGED_RETRY)); }
                }
                _ => if let Some(row) = existing {
                    if def.hidden || def.retired || row.expires_at.saturating_sub(now) > RENEWAL_WINDOW { return Err(notify_error(messages::expert_systems::CANNOT_RENEW)); }
                    let expires = row.expires_at.checked_add(def.duration_filetime).ok_or_else(|| notify_error(messages::expert_systems::DURATION_OVERFLOW))?;
                    if !dao::renew(tx, char_id, type_id, row.expires_at, expires).await.map_err(db_err)? { return Err(notify_error(messages::expert_systems::CHANGED_RETRY)); }
                } else {
                    if rows.len() >= MAX_ACTIVE { return Err(notify_error(messages::expert_systems::ACTIVE_LIMIT)); }
                    let expires = now.checked_add(def.duration_filetime).ok_or_else(|| notify_error(messages::expert_systems::DURATION_OVERFLOW))?;
                    if !dao::insert(tx, char_id, type_id, now, expires).await.map_err(db_err)? { return Err(notify_error(messages::expert_systems::CHANGED_RETRY)); }
                },
            }
            super::training::retime_after_change_tx(tx, &sde, char_id, now, &before, &mut progress).await?;
            // Emit a single final map after skill and brain notifications.
            progress.experts_changed = false;
            progress.brain_changed = true;
            let skills: Vec<_> = def.skills.iter().map(|(id, _)| *id).collect();
            super::completion::finish_tx(tx, outbox, &sde, &fences, char_id, now, &progress, &skills).await?;
            let active = dao::active(tx, char_id, now).await.map_err(db_err)?;
            changed(outbox, char_id, &active, !matches!(action, Action::Remove(_)) && added, type_id);
            Ok(())
        })
    }, |value, outbox| async move { let result = online::dispatch_entries(value, outbox, world, &mut dispatch); crate::scheduler::wake(); result }).await
}

pub(crate) async fn consume<F>(identity: &Identity, db: &db::Db, world: &World, sde: &Arc<Sde>, item_id: i64, dispatch: F) -> Result<(), CallError>
where F: FnMut(Notification) -> Result<(), CallError> + Send { mutate(identity, db, world, sde, Action::Consume(item_id), dispatch).await }

pub(crate) async fn remove<F>(identity: &Identity, db: &db::Db, world: &World, sde: &Arc<Sde>, type_id: i32, dispatch: F) -> Result<(), CallError>
where F: FnMut(Notification) -> Result<(), CallError> + Send { mutate(identity, db, world, sde, Action::Remove(type_id), dispatch).await }

pub(crate) async fn gm<F>(identity: &Identity, db: &db::Db, world: &World, sde: &Arc<Sde>, add: bool, type_id: i32, dispatch: F) -> Result<String, CallError>
where F: FnMut(Notification) -> Result<(), CallError> + Send {
    mutate(identity, db, world, sde, if add { Action::Install(type_id) } else { Action::Remove(type_id) }, dispatch).await?;
    Ok(if add { messages::gm::gm_expert_system_added(type_id) } else { messages::gm::gm_expert_system_removed(type_id) })
}

#[cfg(test)] mod tests;
