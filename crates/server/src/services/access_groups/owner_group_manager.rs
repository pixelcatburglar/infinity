//! `ownerGroupManager` access-group RPCs used by the structure access-group editor.
//! Persists group rows and audit history in game access-group tables; corporation-owned
//! lifecycle and structure-specific policy are not supported.
use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
use marshal::{DbRowDescriptor, DbType, Value};
use infinity_core::{consts::{TYPE_ALLIANCE, TYPE_CHARACTER, TYPE_CORPORATION}, messages::access_groups as msg};
use crate::{client::{Client, filetime_now}, dao::access_groups as dao, db::{executor::Executor, outbox::{dispatch_world, Outbox}}, services::{Call, CallError, CallResult, crowset, keyval, notify_error}};
use super::{membership, lock, ADMIN, EXCLUDED, MANAGER, MEMBER, NONE, PUBLIC};
use crate::services::bookmarks::notifications;

// ownergroups/ownergroupConst.py action and change codes.
const CREATED: i32 = 1;
const ADDED: i32 = 2;
const TYPE_CHANGED: i32 = 3;
const REMOVED: i32 = 4;
const NAME_CHANGED: i32 = 5;
const DESCRIPTION_CHANGED: i32 = 6;
const DELETED: i32 = 7;
const LISTENER_CHANGED: i32 = 8;
// nameAndDescriptionWnd.py entry limits.
const MAX_NAME: usize = 30;
const MAX_DESCRIPTION: usize = 200;

fn invalid() -> CallError { notify_error(msg::INVALID) }
fn denied() -> CallError { notify_error(msg::DENIED) }
fn id(v: Option<&Value>) -> Result<i64, CallError> { v.and_then(Value::as_int).filter(|n| *n > 0).ok_or_else(invalid) }
fn ids(v: Option<&Value>) -> Result<Vec<i64>, CallError> {
    v.and_then(Value::items).ok_or_else(invalid)?.iter().map(|x| x.as_int().ok_or_else(invalid)).collect()
}
fn text(v: Option<&Value>, max: usize, required: bool) -> Result<String, CallError> {
    let text = v.and_then(Value::as_text).ok_or_else(invalid)?;
    let s = if required { text.trim().to_string() } else { text.into_owned() };
    if s.chars().count() > max || (required && s.is_empty()) { return Err(invalid()); }
    Ok(s)
}
fn member_rows(rows: &[dao::Member]) -> Value {
    Value::list(rows.iter().map(|r| keyval([("memberID", Value::Int(r.member_id)), ("membershipType", Value::Int(r.membership_type.into()))])))
}
async fn character_admins(ex: &(impl Executor + ?Sized), sde: &crate::sde::Sde, members: &[dao::Member]) -> Result<BTreeSet<i64>, CallError> {
    let ids: Vec<i64> = members.iter().filter(|r| r.membership_type == ADMIN).map(|r| r.member_id).collect();
    let mut result: BTreeSet<i64> = crate::dao::characters::social::owners(ex, &ids, TYPE_CHARACTER as i64, TYPE_CORPORATION, TYPE_ALLIANCE).await?
        .into_iter().filter(|o| o.type_id == TYPE_CHARACTER as i64).map(|o| o.id).collect();
    result.extend(ids.into_iter().filter(|id| sde.characters.npc_character(*id).is_some()));
    Ok(result)
}
async fn group_row(ex: &(impl Executor + ?Sized), sde: &crate::sde::Sde, group: &dao::Group, me: i64) -> Result<Value, CallError> {
    let members = dao::members(ex, group.group_id).await?;
    let admins = character_admins(ex, sde, &members).await?.into_iter().map(Value::Int);
    Ok(keyval([("groupID", Value::Int(group.group_id)), ("name", Value::unicode(group.name.as_str())),
        ("description", Value::unicode(group.description.as_str())), ("creatorID", Value::Int(group.creator_id)),
        ("membershipType", Value::Int(membership(ex, me, group.group_id).await?.into())), ("admins", Value::list(admins))]))
}
async fn required(ex: &(impl Executor + ?Sized), me: i64, group: i64, minimum: i32) -> Result<i32, CallError> {
    let kind = membership(ex, me, group).await?;
    if kind < minimum || kind == EXCLUDED { return Err(denied()); }
    Ok(kind)
}
async fn lifecycle(ex: &(impl Executor + ?Sized), group: &dao::Group) -> Result<(), CallError> {
    let owners = crate::dao::characters::social::owners(ex, &[group.creator_id], TYPE_CHARACTER as i64, TYPE_CORPORATION, TYPE_ALLIANCE).await?;
    if owners.iter().any(|o| o.type_id == TYPE_CORPORATION) { return Err(notify_error(msg::CORP_OWNED)); }
    Ok(())
}
async fn log(ex: &(impl Executor + ?Sized), group: i64, me: i64, action: i32, owner: Option<i64>, old_text: Option<&str>, new_text: Option<&str>, old: Option<i32>, new: Option<i32>) -> Result<(), CallError> {
    dao::insert_log(ex, &dao::NewLog { group_id: group, log_date: filetime_now(), character_id: me, action_id: action, owner_id: owner, old_text, new_text, old_membership_type: old, new_membership_type: new }).await?;
    Ok(())
}
async fn audience(ex: &(impl Executor + ?Sized), group: i64, online: &[i64]) -> Result<BTreeMap<i64, i32>, CallError> {
    let mut result = BTreeMap::new();
    for &id in online { result.insert(id, membership(ex, id, group).await?); }
    Ok(result)
}
fn notify(outbox: &mut Outbox, group: i64, me: i64, before: &BTreeMap<i64, i32>, after: &BTreeMap<i64, i32>, changes: &[i32]) {
    if changes.is_empty() { return; }
    for (&id, &old) in before {
        let new = after.get(&id).copied().unwrap_or(NONE);
        if id != me && !(old >= MEMBER && old <= ADMIN) && !(new >= MEMBER && new <= ADMIN) { continue; }
        let mut codes = changes.to_vec();
        if old != new { codes.push(LISTENER_CHANGED); }
        outbox.notify_pilot(id, "OnOwnerGroupsUpdated", vec![Value::Dict(vec![(Value::Int(group), Value::list(codes.into_iter().map(|v| Value::Int(v.into()))))])]);
    }
}
fn failure(id: i64, key: &str) -> Value { Value::tuple([Value::Int(id), Value::str(key)]) }

#[derive(Clone)]
enum Operation { Create(String, String), Name(i64, String), Description(i64, String), Delete(i64), Add(i64, Vec<(i64, i32)>), Update(i64, Vec<(i64, i32)>), Remove(i64, Vec<(i64, i32)>) }
impl Operation { fn group(&self) -> Option<i64> { match self { Self::Create(..) => None, Self::Name(g,..)|Self::Description(g,..)|Self::Delete(g)|Self::Add(g,..)|Self::Update(g,..)|Self::Remove(g,..) => Some(*g) } } }

async fn mutate(c: &Client, me: i64, op: Operation) -> CallResult {
    let _guard = lock().await;
    let mut online = c.world.online_pilots();
    online.push(me); online.sort_unstable(); online.dedup();
    let mut static_types = BTreeMap::new();
    if let Operation::Add(_, rows) | Operation::Update(_, rows) | Operation::Remove(_, rows) = &op {
        for &(id, _) in rows {
            if c.sde.characters.npc_character(id).is_some() { static_types.insert(id, TYPE_CHARACTER as i64); }
            else if c.sde.npc_corporations.iter().any(|r| r.corporation_id == id) { static_types.insert(id, TYPE_CORPORATION); }
        }
    }
    let op = Arc::new(op); let online = Arc::new(online); let static_types = Arc::new(static_types);
    c.db.transaction(|tx, outbox: &mut Outbox| {
        let op = Arc::clone(&op); let online = Arc::clone(&online); let static_types = Arc::clone(&static_types); let sde = Arc::clone(&c.sde);
        Box::pin(async move {
            if let Operation::Create(name, description) = op.as_ref() {
                let group = dao::create(tx, me, name, description).await?;
                dao::set_member(tx, group.group_id, me, ADMIN).await?;
                log(tx, group.group_id, me, CREATED, None, None, None, None, None).await?;
                let mut before = BTreeMap::new(); before.insert(me, NONE);
                let mut after = BTreeMap::new(); after.insert(me, ADMIN);
                notify(outbox, group.group_id, me, &before, &after, &[CREATED]);
                return Ok(Value::Int(group.group_id));
            }
            let group_id = op.group().ok_or_else(invalid)?;
            let group = dao::group(tx, group_id).await?.ok_or_else(|| notify_error(msg::MISSING))?;
            let actor = membership(tx, me, group_id).await?;
            let mut old_members = dao::members(tx, group_id).await?;
            let mut admin_ids = character_admins(tx, &sde, &old_members).await?;
            let self_remove = match op.as_ref() { Operation::Remove(_, rows) => rows.iter().all(|(id, _)| *id == me), _ => false };
            if actor != ADMIN && actor != MANAGER && !self_remove { return Err(denied()); }
            if matches!(op.as_ref(), Operation::Name(..)|Operation::Description(..)|Operation::Delete(..)) {
                if actor != ADMIN { return Err(denied()); }
                lifecycle(tx, &group).await?;
            }
            let mut targets = online.as_ref().clone();
            let mut types = BTreeMap::new();
            if let Operation::Add(_, rows) | Operation::Update(_, rows) | Operation::Remove(_, rows) = op.as_ref() {
                let requested: Vec<i64> = rows.iter().map(|(id, _)| *id).filter(|id| *id != PUBLIC).collect();
                let owners = crate::dao::characters::social::owners(tx, &requested, TYPE_CHARACTER as i64, TYPE_CORPORATION, TYPE_ALLIANCE).await?;
                types.extend(owners.into_iter().map(|o| (o.id, o.type_id)));
                types.extend(static_types.iter().map(|(id, kind)| (*id, *kind)));
                targets.extend(types.iter().filter_map(|(&id, &kind)| (kind == TYPE_CHARACTER as i64).then_some(id)));
                targets.sort_unstable();
                targets.dedup();
            }
            let before = audience(tx, group_id, &targets).await?;
            let affects_access = matches!(op.as_ref(), Operation::Delete(..)|Operation::Add(..)|Operation::Update(..)|Operation::Remove(..));
            let snapshots = if affects_access { notifications::snapshot_group(tx, group_id, &targets).await? } else { Vec::new() };
            let mut changes = Vec::new(); let mut failures = Vec::new();
            match op.as_ref() {
                Operation::Name(_, name) if *name != group.name => {
                    dao::update_name(tx, group_id, name).await?;
                    log(tx, group_id, me, NAME_CHANGED, None, Some(&group.name), Some(name), None, None).await?; changes.push(NAME_CHANGED);
                }
                Operation::Description(_, description) if *description != group.description => {
                    dao::update_description(tx, group_id, description).await?;
                    log(tx, group_id, me, DESCRIPTION_CHANGED, None, Some(&group.description), Some(description), None, None).await?; changes.push(DESCRIPTION_CHANGED);
                }
                Operation::Delete(_) => {
                    log(tx, group_id, me, DELETED, None, None, None, None, None).await?;
                    crate::dao::bookmarks::clear_group(tx, group_id).await?; dao::delete(tx, group_id).await?; changes.push(DELETED);
                }
                Operation::Add(_, rows) | Operation::Update(_, rows) | Operation::Remove(_, rows) => {
                    let mut rows = rows.clone(); rows.sort_unstable_by_key(|r| r.0); rows.dedup_by_key(|r| r.0);
                    for (id, new) in rows {
                        let existing = old_members.iter().find(|r| r.member_id == id).map(|r| r.membership_type);
                        let is_character = types.get(&id) == Some(&(TYPE_CHARACTER as i64));
                        let removal = matches!(op.as_ref(), Operation::Remove(..));
                        let adding = matches!(op.as_ref(), Operation::Add(..));
                        let err = if id != PUBLIC && !types.contains_key(&id) { Some(msg::ILLEGAL_MEMBER) }
                        else if !removal && !(MEMBER..=EXCLUDED).contains(&new) { Some(msg::ILLEGAL_MEMBER) }
                        else if !removal && (new == MANAGER || new == ADMIN) && !is_character { Some(msg::CHARACTER_REQUIRED) }
                        else if actor == MANAGER && existing.is_some_and(|x| x == MANAGER || x == ADMIN) && !(removal && id == me) { Some(msg::MANAGER_UPDATE) }
                        else if actor == MANAGER && !removal && (new == MANAGER || new == ADMIN) { Some(if adding { msg::MANAGER_ADD } else { msg::PROMOTION }) }
                        else if actor != ADMIN && !removal && id == me && new > actor && new != EXCLUDED { Some(msg::PROMOTION) }
                        else if existing == Some(ADMIN) && admin_ids.contains(&id) && (removal || new != ADMIN) && admin_ids.len() <= 1 { Some(msg::LAST_ADMIN) }
                        else { None };
                        if let Some(err) = err { failures.push(failure(id, err)); continue; }
                        if removal {
                            if existing.is_none() { continue; }
                            dao::delete_member(tx, group_id, id).await?;
                            log(tx, group_id, me, REMOVED, Some(id), None, None, existing, None).await?;
                            admin_ids.remove(&id);
                            old_members.retain(|r| r.member_id != id); if !changes.contains(&REMOVED) { changes.push(REMOVED); }
                        } else {
                            if (adding && existing.is_some()) || (!adding && existing.is_none()) || existing == Some(new) { continue; }
                            dao::set_member(tx, group_id, id, new).await?;
                            let code = if existing.is_some() { TYPE_CHANGED } else { ADDED };
                            log(tx, group_id, me, code, Some(id), None, None, existing, Some(new)).await?;
                            if let Some(r) = old_members.iter_mut().find(|r| r.member_id == id) { r.membership_type = new; } else { old_members.push(dao::Member { member_id: id, membership_type: new }); }
                            if new == ADMIN { admin_ids.insert(id); } else { admin_ids.remove(&id); }
                            if !changes.contains(&code) { changes.push(code); }
                        }
                    }
                }
                _ => {}
            }
            if !changes.is_empty() {
                if affects_access { notifications::access_changes(tx, outbox, &snapshots).await?; }
                let after = if matches!(op.as_ref(), Operation::Delete(_)) { before.keys().map(|&id| (id, NONE)).collect() } else { audience(tx, group_id, &targets).await? };
                notify(outbox, group_id, me, &before, &after, &changes);
            }
            if matches!(op.as_ref(), Operation::Add(..)|Operation::Update(..)|Operation::Remove(..)) { Ok(Value::list(failures)) } else { Ok(Value::None) }
        })
    }, |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await
}

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    if me <= 0 { return Some(Err(denied())); }
    let _read_guard = if matches!(call.method, "GetMyGroups" | "GetGroup" | "GetGroupsMany" | "GetMembers" | "GetMembersForMultipleGroups" | "GetGroupLogs") { Some(lock().await) } else { None };
    let args = call.args;
    Some(match call.method {
        "Create" => match (text(args.first(), MAX_NAME, true), text(args.get(1), MAX_DESCRIPTION, false)) {
            (Ok(name), Ok(description)) => mutate(c, me, Operation::Create(name, description)).await,
            _ => Err(invalid()),
        },
        "UpdateName" | "UpdateDescription" => {
            let name = call.method == "UpdateName";
            match (id(args.first()), text(args.get(1), if name { MAX_NAME } else { MAX_DESCRIPTION }, name)) {
                (Ok(group), Ok(value)) => mutate(c, me, if name { Operation::Name(group, value) } else { Operation::Description(group, value) }).await,
                _ => Err(invalid()),
            }
        }
        "Delete" => match id(args.first()) { Ok(group) => mutate(c, me, Operation::Delete(group)).await, Err(e) => Err(e) },
        "AddMembers" => match (id(args.first()), args.get(1)) {
            (Ok(group), Some(Value::Dict(entries))) => {
                let rows: Option<Vec<_>> = entries.iter().map(|(k, v)| Some((k.as_int()?, i32::try_from(v.as_int()?).ok()?))).collect();
                match rows { Some(rows) => mutate(c, me, Operation::Add(group, rows)).await, None => Err(invalid()) }
            }
            _ => Err(invalid()),
        },
        "UpdateMembershipTypes" | "DeleteMembers" => {
            let parsed = (|| {
                let group = id(args.first())?;
                let ids = ids(args.get(1))?;
                let kind = if call.method == "DeleteMembers" { NONE } else { i32::try_from(args.get(2).and_then(Value::as_int).ok_or_else(invalid)?).map_err(|_| invalid())? };
                Ok::<_, CallError>((group, ids.into_iter().map(|id| (id, kind)).collect()))
            })();
            match parsed {
                Ok((group, rows)) => mutate(c, me, if call.method == "DeleteMembers" { Operation::Remove(group, rows) } else { Operation::Update(group, rows) }).await,
                Err(e) => Err(e),
            }
        }
        "GetMyGroups" => {
            let mut values = Vec::new();
            let result = async {
                for group in dao::groups(&c.db).await? {
                    let kind = membership(&c.db, me, group.group_id).await?;
                    if (MEMBER..=ADMIN).contains(&kind) { values.push(group_row(&c.db, &c.sde, &group, me).await?); }
                }
                Ok(Value::list(values))
            }.await;
            result
        }
        "GetGroup" => match id(args.first()) {
            Ok(group) => match dao::group(&c.db, group).await {
                Ok(Some(row)) => group_row(&c.db, &c.sde, &row, me).await,
                Ok(None) => Err(notify_error(msg::MISSING)),
                Err(e) => Err(e.into()),
            },
            Err(e) => Err(e),
        },
        "GetGroupsMany" => match ids(args.first()) {
            Ok(ids) => {
                let result = async {
                    let mut rows = Vec::new();
                    for id in ids {
                        if let Some(group) = dao::group(&c.db, id).await? { rows.push(group_row(&c.db, &c.sde, &group, me).await?); }
                    }
                    Ok(Value::list(rows))
                }.await;
                result
            }
            Err(e) => Err(e),
        },
        "GetMembers" | "GetGroupLogs" => match id(args.first()) {
            Ok(group) => {
                let result = async {
                    required(&c.db, me, group, MEMBER).await?;
                    if call.method == "GetMembers" { return Ok(member_rows(&dao::members(&c.db, group).await?)); }
                    let header = DbRowDescriptor::new([
                        ("logDate", DbType::FileTime), ("actionID", DbType::I4), ("characterID", DbType::I8),
                        ("ownerID", DbType::I8), ("oldText", DbType::WStr), ("newText", DbType::WStr),
                        ("oldMembershipType", DbType::I4), ("newMembershipType", DbType::I4),
                    ]);
                    let lines = dao::logs(&c.db, group).await?.into_iter().map(|r| header.row(&[
                        Value::Int(r.log_date), Value::Int(r.action_id.into()), Value::Int(r.character_id),
                        r.owner_id.map(Value::Int).unwrap_or(Value::None),
                        r.old_text.map(Value::unicode).unwrap_or(Value::None), r.new_text.map(Value::unicode).unwrap_or(Value::None),
                        r.old_membership_type.map(|v| Value::Int(v.into())).unwrap_or(Value::None),
                        r.new_membership_type.map(|v| Value::Int(v.into())).unwrap_or(Value::None),
                    ])).collect();
                    Ok(crowset(&header, lines))
                }.await;
                result
            }
            Err(e) => Err(e),
        },
        "GetMembersForMultipleGroups" => match ids(args.first()) {
            Ok(ids) => {
                let result = async {
                    for &group in &ids { required(&c.db, me, group, MEMBER).await?; }
                    let mut result = Vec::new();
                    for group in ids { result.push((Value::Int(group), member_rows(&dao::members(&c.db, group).await?))); }
                    Ok(Value::Dict(result))
                }.await;
                result
            }
            Err(e) => Err(e),
        },
        _ => return None,
    })
}
