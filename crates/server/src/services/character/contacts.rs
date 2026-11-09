//! Personal address-book mechanics for `charMgr`, matching
//! `eve/client/script/ui/shared/neocom/addressBook/addressbookService.py`.
//! Uses `game.char_contacts`, `game.char_blocked`, owner data and typed
//! `game.notifications`. The client may send a None contact message. Contact label management is not supported.

use super::{char_id, db_err, keyval, rowset, CallError, CallResult, Client, Value};
use infinity_core::messages;
use crate::{dao::characters::{contacts as dao, social}, db::outbox::{dispatch_world, Outbox}, services::{mail, notify_error}};
use std::sync::Arc;

/// Client notificationconst.py: notificationTypeContactAdd.
const CONTACT_ADD: i32 = 89;
/// Client notificationconst.py: notificationTypeContactEdit.
const CONTACT_EDIT: i32 = 90;
/// Client notificationconst.py: groupContacts.
const CONTACT_GROUP: i32 = 9;
/// Client contact relationship range.
const MAX_RELATIONSHIP: f64 = 10.0;
/// Owner-kind tags passed to the existing owner resolver (not SDE type IDs).
const CHARACTER: i64 = 1;
const CORPORATION: i64 = 2;
const ALLIANCE: i64 = 3;

fn invalid() -> CallError { notify_error(messages::contacts::INVALID) }
fn id(value: &Value) -> Result<i64, CallError> {
    match value { Value::Int(id) if *id > 0 => Ok(*id), _ => Err(invalid()) }
}
fn ids(value: &Value) -> Result<Vec<i64>, CallError> {
    value.items().ok_or_else(invalid)?.iter().map(id).collect()
}
fn relationship(value: &Value) -> Result<f32, CallError> {
    let n = value.as_float().ok_or_else(invalid)?;
    if !n.is_finite() || !(-MAX_RELATIONSHIP..=MAX_RELATIONSHIP).contains(&n) { return Err(invalid()); }
    Ok(n as f32)
}
fn boolean(value: &Value) -> Result<bool, CallError> {
    match value { Value::Bool(b) => Ok(*b), Value::Int(0) => Ok(false), Value::Int(1) => Ok(true), _ => Err(invalid()) }
}
struct ContactInput { id: i64, relationship: f32, watchlist: bool, notify: bool, message: String, agent: bool }
fn parse_contact(args: &[Value], edit: bool) -> Result<ContactInput, CallError> {
    if !edit && args.len() == 1 {
        return Ok(ContactInput { id: id(&args[0])?, relationship: 0.0, watchlist: false, notify: false, message: String::new(), agent: true });
    }
    if args.len() != 5 { return Err(invalid()); }
    let message = match &args[4] { Value::None => String::new(), value => value.as_text().ok_or_else(invalid)?.into_owned() };
    Ok(ContactInput { id: id(&args[0])?, relationship: relationship(&args[1])?, watchlist: boolean(&args[2])?, notify: boolean(&args[3])?, message, agent: false })
}
fn payload(relationship: f32, message: &str) -> String {
    serde_json::json!({"level": relationship, "message": message}).to_string()
}
fn contact_list(rows: Vec<dao::Contact>, blocked: Vec<i64>) -> Value {
    keyval([
        ("addresses", rowset(&["contactID", "inWatchlist", "relationshipID", "labelMask"], rows.into_iter().map(|r| vec![Value::Int(r.contact_id), Value::Bool(r.in_watchlist), Value::Float(r.relationship as f64), Value::Int(r.label_mask)]).collect())),
        ("blocked", rowset(&["senderID"], blocked.into_iter().map(|id| vec![Value::Int(id)]).collect())),
    ])
}
fn updated(outbox: &mut Outbox, me: i64, ids: Vec<i64>, relationship: Option<f32>, watchlist: Option<bool>) {
    if ids.is_empty() { return; }
    outbox.notify_pilot(me, "OnPersonalContactsUpdated", vec![Value::list(ids.into_iter().map(Value::Int)), relationship.map(|r| Value::Float(r as f64)).unwrap_or(Value::None), watchlist.map(Value::Bool).unwrap_or(Value::None)]);
}

pub(super) async fn list(c: &Client, args: &[Value]) -> CallResult {
    if !args.is_empty() || char_id(c) <= 0 { return Err(invalid()); }
    let me = char_id(c);
    Ok(contact_list(dao::list(&c.db, me).await.map_err(db_err)?, dao::blocked(&c.db, me).await.map_err(db_err)?))
}

pub(super) async fn add_edit(c: &Client, args: &[Value], edit: bool) -> CallResult {
    let input = parse_contact(args, edit)?;
    let me = char_id(c);
    if me <= 0 || input.id == me { return Err(invalid()); }
    let npc = c.sde.characters.npc_character(input.id).is_some();
    let agent = crate::services::missions::agent(input.id).is_some();
    if input.agent && !agent { return Err(invalid()); }
    let static_owner = npc || agent || c.sde.npc_corporations.iter().any(|r| r.corporation_id == input.id) || c.sde.factions.iter().any(|r| r.faction_id == input.id);
    let input = Arc::new(input);
    c.db.transaction(|tx, outbox: &mut Outbox| {
        let input = Arc::clone(&input);
        Box::pin(async move {
            let owners = social::owners(tx, &[input.id], CHARACTER, CORPORATION, ALLIANCE).await.map_err(db_err)?;
            if owners.is_empty() && !static_owner { return Err(notify_error(messages::contacts::UNKNOWN)); }
            dao::upsert(tx, me, input.id, input.relationship, input.watchlist).await.map_err(db_err)?;
            updated(outbox, me, vec![input.id], Some(input.relationship), Some(input.watchlist));
            if input.notify && !npc && owners.iter().any(|owner| owner.type_id == CHARACTER) {
                mail::notify(tx, outbox, input.id, if edit { CONTACT_EDIT } else { CONTACT_ADD }, CONTACT_GROUP, me, &payload(input.relationship, &input.message)).await?;
            }
            Ok(Value::None)
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}

#[derive(Clone, Copy)]
pub(super) enum Batch { Relationship, Delete, Block(bool) }
pub(super) async fn batch(c: &Client, args: &[Value], operation: Batch) -> CallResult {
    let expected = if matches!(operation, Batch::Relationship) { 2 } else { 1 };
    if args.len() != expected || char_id(c) <= 0 { return Err(invalid()); }
    let ids = ids(&args[0])?;
    let standing = if expected == 2 { Some(relationship(&args[1])?) } else { None };
    let me = char_id(c);
    if matches!(operation, Batch::Block(true)) && ids.contains(&me) { return Err(invalid()); }
    if matches!(operation, Batch::Block(true)) {
        let owners = social::owners(&c.db, &ids, CHARACTER, CORPORATION, ALLIANCE).await.map_err(db_err)?;
        if ids.iter().any(|id| !owners.iter().any(|r| r.id == *id) && c.sde.characters.npc_character(*id).is_none() && !c.sde.npc_corporations.iter().any(|r| r.corporation_id == *id) && !c.sde.factions.iter().any(|r| r.faction_id == *id)) { return Err(notify_error(messages::contacts::UNKNOWN)); }
    }
    let ids = Arc::new(ids);
    c.db.transaction(|tx, outbox: &mut Outbox| {
        let ids = Arc::clone(&ids);
        Box::pin(async move {
            match operation {
                Batch::Relationship => {
                    let r = standing.expect("relationship argument");
                    let rows = dao::edit_relationships(tx, me, &ids, r).await.map_err(db_err)?;
                    updated(outbox, me, rows.into_iter().map(|r| r.contact_id).collect(), Some(r), None);
                }
                Batch::Delete => {
                    let rows = dao::delete(tx, me, &ids).await.map_err(db_err)?;
                    updated(outbox, me, rows.into_iter().map(|r| r.contact_id).collect(), None, None);
                }
                Batch::Block(block) => {
                    for row in dao::set_blocked(tx, me, &ids, block).await.map_err(db_err)? {
                        updated(outbox, me, vec![row.contact_id], Some(row.relationship), Some(row.in_watchlist));
                    }
                }
            }
            Ok(Value::None)
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}

#[cfg(test)] mod tests;
