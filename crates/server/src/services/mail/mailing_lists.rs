//! Mailing-list mechanics for `eve/client/script/ui/services/mail/mailingListsSvc.py`.
//! Metadata, members and entity ACLs live in `game.mailing_lists`,
//! `game.mailing_list_members` and `game.mailing_list_access`. Character ACLs take
//! precedence over corporation, alliance and default; operators may manage peers,
//! and kicking adds a character block. Lists are free and deletion leaves a name
//! tombstone for historic mail. PostgreSQL name folding can differ from Python 2's
//! Unicode implementation; normalization is performed only by the DAO.

use super::{delivery::{self, Delivery}, ACCESS_ALLOWED, ACCESS_BLOCKED, MAX_LIST_NAME, MAX_MEMBER_BATCH, MAX_MEMBERS, ROLE_MEMBER, ROLE_MUTED, ROLE_OPERATOR};
use crate::{client::{Client, filetime_now}, consts::FILETIME_MINUTE, dao::{self, mailing_lists as lists}, db::{self, executor::Executor, outbox::{dispatch_world, Outbox}}, messages::mail as msg, services::{db_err, notify_error, CallError}};
use marshal::Value;
use std::sync::Arc;

/// `game.mailing_list_access.entity_kind`: character.
const CHARACTER: i16 = 1;
/// `game.mailing_list_access.entity_kind`: corporation.
const CORPORATION: i16 = 2;
/// `game.mailing_list_access.entity_kind`: alliance.
const ALLIANCE: i16 = 3;

fn selected(c: &Client) -> Result<i64, CallError> {
    c.session.int("charid").filter(|id| *id > 0).ok_or_else(|| notify_error(msg::SELECT_CHARACTER))
}
async fn active_character(ex: &(impl Executor + ?Sized), me: i64) -> Result<(), CallError> {
    if dao::characters::lookup::find(ex, "", Some(me)).await.map_err(db_err)?.is_none() {
        return Err(notify_error(msg::SELECT_CHARACTER));
    }
    Ok(())
}
async fn locked(ex: &(impl Executor + ?Sized), id: i64) -> Result<lists::List, CallError> {
    lists::get_for_update(ex, id).await.map_err(db_err)?.filter(|list| list.deleted_at.is_none()).ok_or_else(|| notify_error(msg::LIST_NOT_FOUND))
}
async fn manager(ex: &(impl Executor + ?Sized), list: &lists::List, me: i64) -> Result<(), CallError> {
    if list.owner_id == me || lists::membership(ex, list.list_id, me).await.map_err(db_err)? == Some(ROLE_OPERATOR) { Ok(()) }
    else { Err(notify_error(msg::PERMISSION_DENIED)) }
}
pub(super) async fn authorize_sender(ex: &(impl Executor + ?Sized), list: &lists::List, me: i64) -> Result<(), CallError> {
    if list.deleted_at.is_some() { return Err(notify_error(msg::LIST_NOT_FOUND)); }
    active_character(ex, me).await?;
    if list.owner_id == me { return Ok(()); }
    match lists::membership(ex, list.list_id, me).await.map_err(db_err)? {
        Some(ROLE_MUTED) => Err(notify_error(msg::MUTED)),
        Some(_) => Ok(()),
        None => Err(notify_error(msg::PERMISSION_DENIED)),
    }
}
fn defaults(access: i16, role: i16, cost: i64) -> Result<(), CallError> {
    if cost != 0 { return Err(notify_error(msg::FEES_UNSUPPORTED)); }
    if ![ACCESS_BLOCKED, ACCESS_ALLOWED].contains(&access) || ![ROLE_MUTED, ROLE_MEMBER].contains(&role) { return Err(notify_error(msg::INVALID_ARGUMENT)); }
    Ok(())
}
fn name_error(error: db::Error) -> CallError {
    if matches!(&error, db::Error::Postgres(e) if e.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)) { notify_error(msg::NAME_TAKEN) } else { db_err(error) }
}
async fn entity_kind(sde: &crate::sde::Sde, ex: &(impl Executor + ?Sized), id: i64) -> Result<i16, CallError> {
    if id <= 0 { return Err(notify_error(msg::INVALID_ENTITY)); }
    if sde.characters.npc_character(id).is_some() || dao::characters::lookup::find(ex, "", Some(id)).await.map_err(db_err)?.is_some() { return Ok(CHARACTER); }
    if sde.npc_corporations.get(id).is_some() || dao::corporations::lookup::find(ex, "", Some(id)).await.map_err(db_err)?.is_some() { return Ok(CORPORATION); }
    if dao::alliances::exists(ex, id).await.map_err(db_err)? { return Ok(ALLIANCE); }
    Err(notify_error(msg::INVALID_ENTITY))
}
async fn welcome(ex: &(impl Executor + ?Sized), outbox: &mut Outbox, list: &lists::List, title: String, body: String, recipients: Vec<i64>) -> Result<(), CallError> {
    let sent = filetime_now() / FILETIME_MINUTE * FILETIME_MINUTE;
    delivery::deliver(ex, outbox, &Delivery { sender: list.list_id, to_chars: Vec::new(), to_list: Some(list.list_id), to_corp: None, title, body, sent, recipients, automated: true }).await?;
    Ok(())
}

pub(super) async fn create(c: &Client, name: String, access: i16, role: i16, cost: i64) -> Result<i64, CallError> {
    let me = selected(c)?; defaults(access, role, cost)?;
    if name.chars().count() > MAX_LIST_NAME { return Err(notify_error(msg::INVALID_NAME)); }
    c.db.transaction(|tx, _: &mut Outbox| {
        let name = name.clone();
        Box::pin(async move {
            active_character(tx, me).await?;
            let (key, display) = lists::normalize_name(tx, &name).await.map_err(db_err)?;
            if key.is_empty() || key.chars().count() > MAX_LIST_NAME || display.is_empty() || display.chars().count() > MAX_LIST_NAME { return Err(notify_error(msg::INVALID_NAME)); }
            let now = filetime_now();
            let list = lists::create(tx, &key, &display, me, access, role, now).await.map_err(name_error)?;
            lists::insert_member_if_absent(tx, list.list_id, me, ROLE_MEMBER, now).await.map_err(db_err)?;
            Ok(list.list_id)
        })
    }, |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn join(c: &Client, name: String) -> Result<(lists::List, i16), CallError> {
    let me = selected(c)?;
    if name.chars().count() > MAX_LIST_NAME { return Err(notify_error(msg::INVALID_NAME)); }
    c.db.transaction(|tx, outbox: &mut Outbox| {
        let name = name.clone();
        Box::pin(async move {
            active_character(tx, me).await?;
            let (key, _) = lists::normalize_name(tx, &name).await.map_err(db_err)?;
            if key.is_empty() || key.chars().count() > MAX_LIST_NAME { return Err(notify_error(msg::INVALID_NAME)); }
            let found = lists::find_active_by_key(tx, &key).await.map_err(db_err)?.ok_or_else(|| notify_error(msg::LIST_NOT_FOUND))?;
            let list = locked(tx, found.list_id).await?;
            if list.name_key != key { return Err(notify_error(msg::LIST_NOT_FOUND)); }
            if let Some(role) = lists::membership(tx, list.list_id, me).await.map_err(db_err)? { return Ok((list, role)); }
            if list.owner_id != me {
                let corp = dao::characters::crimewatch::corporation(tx, me).await.map_err(db_err)?;
                let alliance = match corp { Some(id) => dao::alliances::live_corporation_alliance(tx, id).await.map_err(db_err)?.flatten(), None => None };
                let entries = lists::access_entries(tx, list.list_id).await.map_err(db_err)?;
                let access = [(Some(me), CHARACTER), (corp, CORPORATION), (alliance, ALLIANCE)].into_iter().find_map(|(id, kind)| id.and_then(|id| entries.iter().find(|e| e.entity_id == id && e.entity_kind == kind).map(|e| e.access))).unwrap_or(list.default_access);
                if access != ACCESS_ALLOWED { return Err(notify_error(msg::ACCESS_DENIED)); }
            }
            if lists::member_count(tx, list.list_id).await.map_err(db_err)? >= MAX_MEMBERS as i64 { return Err(notify_error(msg::LIST_FULL)); }
            let role = if list.owner_id == me { ROLE_MEMBER } else { list.default_member_access };
            lists::insert_member_if_absent(tx, list.list_id, me, role, filetime_now()).await.map_err(db_err)?;
            if let (Some(title), Some(body)) = (&list.welcome_title, &list.welcome_body) { welcome(tx, outbox, &list, title.clone(), body.clone(), vec![me]).await?; }
            Ok((list, role))
        })
    }, |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn get_info(c: &Client, id: i64) -> Result<Option<lists::List>, CallError> {
    let me = selected(c)?; active_character(&c.db, me).await?;
    lists::get_info(&c.db, id).await.map_err(db_err)
}
pub(super) async fn joined(c: &Client) -> Result<Vec<(lists::List, i16)>, CallError> {
    let me = selected(c)?; active_character(&c.db, me).await?;
    lists::joined_lists(&c.db, me).await.map_err(db_err)
}
pub(super) async fn get_members(c: &Client, id: i64) -> Result<(lists::List, Vec<lists::Member>), CallError> {
    let me = selected(c)?;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        let rows = lists::members(tx, id).await.map_err(db_err)?; Ok((list, rows))
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn get_settings(c: &Client, id: i64) -> Result<(lists::List, Vec<lists::Access>), CallError> {
    let me = selected(c)?;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        let rows = lists::access_entries(tx, id).await.map_err(db_err)?; Ok((list, rows))
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn get_welcome(c: &Client, id: i64) -> Result<Option<(String, String)>, CallError> {
    let me = selected(c)?;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        Ok(list.welcome_title.zip(list.welcome_body))
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn leave(c: &Client, id: i64) -> Result<(), CallError> {
    let me = selected(c)?;
    c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?;
        if list.owner_id == me { return Err(notify_error(msg::OWNER_PROTECTED)); }
        if lists::membership(tx, id, me).await.map_err(db_err)?.is_none() { return Err(notify_error(msg::INVALID_MEMBER)); }
        lists::remove_members(tx, id, &[me]).await.map_err(db_err)?;
        outbox.notify_pilot(me, "OnMailingListLeave", vec![Value::Int(id), Value::Int(me)]); Ok(())
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn delete(c: &Client, id: i64) -> Result<(), CallError> {
    let me = selected(c)?;
    c.db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?;
        if list.owner_id != me { return Err(notify_error(msg::PERMISSION_DENIED)); }
        let recipients = lists::recipient_ids(tx, id).await.map_err(db_err)?;
        lists::mark_deleted(tx, id, filetime_now()).await.map_err(db_err)?;
        lists::clear_members(tx, id).await.map_err(db_err)?; lists::clear_all_access(tx, id).await.map_err(db_err)?;
        for recipient in recipients { outbox.notify_pilot(recipient, "OnMailingListDeleted", vec![Value::Int(id)]); } Ok(())
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
#[derive(Clone, Copy)]
pub(super) enum MemberAction { Kick, Muted, Operator, Clear }
pub(super) async fn members_action(c: &Client, id: i64, mut ids: Vec<i64>, action: MemberAction) -> Result<(), CallError> {
    let me = selected(c)?;
    if ids.len() > MAX_MEMBER_BATCH || ids.iter().any(|id| *id <= 0) { return Err(notify_error(msg::INVALID_ARGUMENT)); }
    ids.sort_unstable(); ids.dedup(); let ids = Arc::new(ids);
    c.db.transaction(|tx, outbox: &mut Outbox| { let ids = Arc::clone(&ids); Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        if ids.contains(&list.owner_id) { return Err(notify_error(msg::OWNER_PROTECTED)); }
        let members = lists::members(tx, id).await.map_err(db_err)?;
        if ids.iter().any(|id| !members.iter().any(|m| m.character_id == *id)) { return Err(notify_error(msg::INVALID_MEMBER)); }
        match action {
            MemberAction::Kick => {
                lists::remove_members(tx, id, &ids).await.map_err(db_err)?;
                for &target in ids.iter() { lists::upsert_access(tx, id, target, CHARACTER, ACCESS_BLOCKED).await.map_err(db_err)?; outbox.notify_pilot(target, "OnMailingListLeave", vec![Value::Int(id), Value::Int(target)]); }
            }
            _ => {
                let (role, event) = match action { MemberAction::Muted => (ROLE_MUTED, "OnMailingListSetMuted"), MemberAction::Operator => (ROLE_OPERATOR, "OnMailingListSetOperator"), MemberAction::Clear => (ROLE_MEMBER, "OnMailingListSetClear"), MemberAction::Kick => unreachable!() };
                for target in lists::set_member_access(tx, id, &ids, role).await.map_err(db_err)? { outbox.notify_pilot(target, event, vec![Value::Int(id)]); }
            }
        } Ok(())
    }) }, |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn set_access(c: &Client, id: i64, mut entries: Vec<(i64, i16)>) -> Result<(), CallError> {
    let me = selected(c)?;
    if entries.len() > MAX_MEMBER_BATCH || entries.iter().any(|(id, access)| *id <= 0 || ![ACCESS_BLOCKED, ACCESS_ALLOWED].contains(access)) { return Err(notify_error(msg::INVALID_ARGUMENT)); }
    entries.sort_unstable();
    if entries.windows(2).any(|w| w[0].0 == w[1].0 && w[0].1 != w[1].1) { return Err(notify_error(msg::INVALID_ARGUMENT)); }
    entries.dedup(); let entries = Arc::new(entries);
    let sde = Arc::clone(&c.sde);
    c.db.transaction(|tx, _: &mut Outbox| { let entries = Arc::clone(&entries); let sde = Arc::clone(&sde); Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        let mut kinds = Vec::with_capacity(entries.len());
        for &(entity, _) in entries.iter() { kinds.push(entity_kind(&sde, tx, entity).await?); }
        for (&(entity, access), kind) in entries.iter().zip(kinds) { lists::upsert_access(tx, id, entity, kind, access).await.map_err(db_err)?; } Ok(())
    }) }, |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn clear_access(c: &Client, id: i64, entity: i64) -> Result<(), CallError> {
    let me = selected(c)?;
    let sde = Arc::clone(&c.sde);
    c.db.transaction(|tx, _: &mut Outbox| { let sde = Arc::clone(&sde); Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        entity_kind(&sde, tx, entity).await?; lists::clear_access(tx, id, entity).await.map_err(db_err)?; Ok(())
    }) }, |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn set_defaults(c: &Client, id: i64, access: i16, role: i16, cost: i64) -> Result<(), CallError> {
    let me = selected(c)?; defaults(access, role, cost)?;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        lists::update_defaults(tx, id, access, role).await.map_err(db_err)?; Ok(())
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn save_welcome(c: &Client, id: i64, title: String, body: String, send: bool) -> Result<(), CallError> {
    let me = selected(c)?; delivery::validate_content(&title, &body)?;
    if title.is_empty() && !body.is_empty() { return Err(notify_error(msg::INVALID_CONTENT)); }
    c.db.transaction(|tx, outbox: &mut Outbox| { let title = title.clone(); let body = body.clone(); Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        if title.is_empty() { lists::clear_welcome(tx, id).await.map_err(db_err)?; }
        else {
            lists::save_welcome(tx, id, &title, &body).await.map_err(db_err)?;
            if send { let recipients = lists::recipient_ids(tx, id).await.map_err(db_err)?; welcome(tx, outbox, &list, title, body, recipients).await?; }
        } Ok(())
    }) }, |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
pub(super) async fn clear_welcome(c: &Client, id: i64) -> Result<(), CallError> {
    let me = selected(c)?;
    c.db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        active_character(tx, me).await?; let list = locked(tx, id).await?; manager(tx, &list, me).await?;
        lists::clear_welcome(tx, id).await.map_err(db_err)?; Ok(())
    }), |v, o| async move { dispatch_world(v, o, &c.world) }).await
}
