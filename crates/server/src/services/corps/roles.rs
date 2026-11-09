//! Corporation role, title and leadership mechanics for corpRegistry.
//! Mirrors eve/client/script/ui/services/corporation, using game corporations,
//! characters, skills and titles, and startup SDE schools and NPC corporations. CEO eligibility
//! uses effective management skill; share accounting is owned by the separate share ledger.
//! Final dissolution refuses outstanding corporate market orders under the market lock.

use super::*;
use crate::dao::corporations::roles as role_dao;

// ---------------------------------------------------------------- roles and titles

/// Decode title rows using the supplied columns.
pub(super) fn title_rowset(value: Option<&Value>) -> Result<Vec<Vec<Value>>, CallError> {
    decode_rowset(value, TITLE_HEADER)
}

/// Decode member rows by their supplied header rather than positional coincidence.
pub(super) fn member_rowset(value: Option<&Value>) -> Result<Vec<Vec<Value>>, CallError> {
    let fields = ["characterID", "title", "divisionID", "squadronID", "roles", "grantableRoles", "rolesAtHQ", "grantableRolesAtHQ", "rolesAtBase", "grantableRolesAtBase", "rolesAtOther", "grantableRolesAtOther", "baseID", "titleMask", "blockRoles"];
    decode_rowset(value, &fields)
}

fn decode_rowset(value: Option<&Value>, fields: &[&str]) -> Result<Vec<Vec<Value>>, CallError> {
    let invalid = || notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES);
    let value = value.ok_or_else(invalid)?;
    let state = match value { Value::Instance(_, state) => state.as_ref(), other => other };
    let header = state.get("header").and_then(Value::items).ok_or_else(invalid)?;
    let lines = state.get("lines").and_then(Value::items).ok_or_else(invalid)?;
    let mut indices = Vec::with_capacity(header.len());
    for field in header {
        let name = field.as_text().ok_or_else(invalid)?;
        let index = fields.iter().position(|field| *field == name).ok_or_else(invalid)?;
        if indices.contains(&index) { return Err(invalid()); }
        indices.push(index);
    }
    if !indices.contains(&0) { return Err(invalid()); }
    let mut result = Vec::with_capacity(lines.len());
    for line in lines {
        let values = line.items().ok_or_else(invalid)?;
        if values.len() != indices.len() { return Err(invalid()); }
        let mut row = vec![Value::None; fields.len()];
        for (&index, value) in indices.iter().zip(values) { row[index] = value.clone(); }
        result.push(row);
    }
    Ok(result)
}
fn integer(value: &Value) -> Option<i64> {
    if let Value::Int(value) = value { Some(*value) } else { None }
}


/// Update one member with current, transaction-locked scoped authority.
pub(super) async fn update_member(db: &Db, world: &World, sde: &Arc<Sde>, me: i64, bound: i64, args: &[Value]) -> Result<(), CallError> {
    update_members(db, world, sde, me, bound, vec![args.to_vec()], Vec::new()).await
}

pub(super) async fn update_members(db: &Db, world: &World, sde: &Arc<Sde>, me: i64, bound: i64, mut lines: Vec<Vec<Value>>, actions: Vec<(usize, i64, i64)>) -> Result<(), CallError> {
    let invalid = || notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES);
    if lines.iter().any(|line| line.len() != 15 || integer(&line[0]).is_none_or(|id| id <= 0)) { return Err(invalid()); }
    lines.sort_by_key(|line| integer(&line[0]).unwrap_or(0));
    if lines.windows(2).any(|pair| integer(&pair[0][0]) == integer(&pair[1][0])) { return Err(invalid()); }
    let mut ids: Vec<i64> = lines.iter().filter_map(|line| integer(&line[0])).collect();
    ids.push(me);
    // One operation time is shared by transaction retries.
    let now = filetime_now();
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let lines = lines.clone();
            let ids = ids.clone();
            let actions = actions.clone();
            let sde = sde.clone();
            Box::pin(async move {
                if !registry::lock_admission(tx, &[], &[bound], &ids).await.map_err(db_err)? { return Err(invalid()); }
                let before: std::collections::HashMap<i64, _> = crate::dao::characters::roles::authorities(tx, &ids).await.map_err(db_err)?.into_iter().map(|row| (row.character, row)).collect();
                let direct: std::collections::HashMap<i64, _> = role_dao::members(tx, &ids).await.map_err(db_err)?.into_iter().map(|row| (row.character, row)).collect();
                let actor = before.get(&me).ok_or_else(invalid)?;
                if !actor.live || actor.corporation != bound { return Err(invalid()); }
                let director = actor.roles[0] & ROLE_DIRECTOR != 0;
                let ceo = permissions::active_ceo(actor, me);
                let grants = if director {
                    [if ceo { sde.corporation_roles.global } else { sde.corporation_roles.global & !ROLE_DIRECTOR },
                     sde.corporation_roles.local, sde.corporation_roles.local, sde.corporation_roles.local]
                } else { actor.grantable };
                let audience = crate::dao::characters::members::active_ids(tx, bound).await.map_err(db_err)?;
                let mut member_changes = Vec::with_capacity(lines.len());
                for input in &lines {
                    let mut line = input.clone();
                    let target = integer(&line[0]).ok_or_else(invalid)?;
                    let before = before.get(&target).ok_or_else(invalid)?;
                    if !before.live || before.corporation != bound { return Err(invalid()); }
                    let mut member = direct.get(&target).cloned().ok_or_else(invalid)?;
                    let old_member = member.clone();
                    for &(index, verb, value) in &actions {
                        let current = line[index].as_int().unwrap_or_else(|| match index {
                            4 | 6 | 8 | 10 => member.roles[(index - 4) / 2],
                            5 | 7 | 9 | 11 => member.grantable[(index - 5) / 2],
                            12 => member.base.unwrap_or(0),
                            13 => member.title_mask,
                            _ => 0,
                        });
                        let next = match verb { CTV_ADD => current | value, CTV_REMOVE => current & !value, CTV_SET => value, _ => return Err(invalid()) };
                        line[index] = Value::Int(next);
                    }
                    let number = |index: usize| -> Result<Option<i64>, CallError> {
                        match &line[index] { Value::None => Ok(None), Value::Int(value) => Ok(Some(*value)), Value::Bool(value) if index == 14 => Ok(Some(i64::from(*value))), _ => Err(invalid()) }
                    };
                    if let Some(value) = number(2)? {
                        if value != member.division && (!director || (before.ceo == Some(target) && !ceo)) { return Err(invalid()); }
                        member.division = value;
                    }
                    if let Some(value) = number(3)? {
                        if value != member.squadron && (!director || (before.ceo == Some(target) && !ceo)) { return Err(invalid()); }
                        member.squadron = value;
                    }
                    let blocking = number(14)?;
                    if blocking.is_some_and(|b| b != 0 && b != 1) || (blocking.is_some() && target != me) { return Err(invalid()); }
                    let self_clear = target == me && blocking == Some(1);
                    let self_unblock = target == me && blocking == Some(0);
                    if self_clear && before.ceo == Some(target) { return Err(invalid()); }
                    let target_director = before.roles[0] & ROLE_DIRECTOR != 0;
                    let target_ceo = before.ceo == Some(target);
                    for scope in 0..4 {
                        for is_grantable in [false, true] {
                            let index = 4 + scope * 2 + usize::from(is_grantable);
                            let Some(new) = number(index)? else { continue };
                            let old = if is_grantable { member.grantable[scope] } else { member.roles[scope] };
                            let added = new & !old;
                            let valid = if scope == 0 { sde.corporation_roles.global } else {
                                sde.corporation_roles.groups.iter().filter(|group| group.scope == scope).fold(0, |mask, group| mask | group.mask)
                            } & sde.corporation_roles.recognized;
                            if added & !valid != 0 || (member.blocked && added != 0) { return Err(invalid()); }
                            let mut changed = old ^ new;
                            while changed != 0 {
                                let role = 1i64 << changed.trailing_zeros();
                                let legacy_removal = director && !target_ceo && !target_director && role & sde.corporation_roles.recognized == 0 && old & role != 0 && new & role == 0;
                                if !(self_clear && new == 0) && !legacy_removal && !permissions::can_edit_role(role, is_grantable, target_ceo, target_director, ceo, director, scope, actor.base, before.base, grants) { return Err(invalid()); }
                                changed &= !role;
                            }
                            if is_grantable { member.grantable[scope] = new; } else { member.roles[scope] = new; }
                        }
                    }
                    if let Some(base) = number(12)? {
                        let next = (base != 0).then_some(base);
                        if next != member.base {
                            if !permissions::can_edit_base(ceo, director) || base < 0 || next.is_some_and(|id| sde.map.station(id).is_none()) { return Err(invalid()); }
                            member.base = next;
                        }
                    }
                    if let Some(mask) = number(13)? {
                        if (mask != member.title_mask && (!(director || (self_clear && mask == 0)) || (target_ceo && !ceo)))
                            || (member.blocked && mask & !member.title_mask != 0) { return Err(invalid()); }
                        member.title_mask = mask;
                    }
                    if let Some(text) = line[1].as_text() {
                        if text.as_ref() != member.title.as_str() && (!director || (target_ceo && !ceo)) { return Err(invalid()); }
                        member.title = text.into_owned();
                    } else if !matches!(line[1], Value::None) { return Err(invalid()); }
                    if let Some(value) = blocking { member.blocked = value != 0; }
                    if self_clear && (member.roles.iter().any(|&v| v != 0) || member.grantable.iter().any(|&v| v != 0) || member.title_mask != 0) { return Err(invalid()); }
                    if self_unblock && line.iter().enumerate().any(|(i, v)| i != 0 && i != 14 && !matches!(v, Value::None)) { return Err(invalid()); }
                    if !role_dao::write_member(tx, target, bound, &member).await.map_err(db_err)? { return Err(invalid()); }
                    let mut changes = Vec::new();
                    let role_names = ["roles", "rolesAtHQ", "rolesAtBase", "rolesAtOther"];
                    let grant_names = ["grantableRoles", "grantableRolesAtHQ", "grantableRolesAtBase", "grantableRolesAtOther"];
                    for scope in 0..4 {
                        for (name, old, new) in [(role_names[scope], old_member.roles[scope], member.roles[scope]), (grant_names[scope], old_member.grantable[scope], member.grantable[scope])] {
                            if old != new { changes.push((Value::str(name), Value::tuple([Value::Int(old), Value::Int(new)]))); }
                        }
                    }
                    let option = |value: Option<i64>| value.map(Value::Int).unwrap_or(Value::None);
                    for (name, old, new) in [
                        ("divisionID", Value::Int(old_member.division), Value::Int(member.division)),
                        ("squadronID", Value::Int(old_member.squadron), Value::Int(member.squadron)),
                        ("baseID", option(old_member.base), option(member.base)),
                        ("titleMask", Value::Int(old_member.title_mask), Value::Int(member.title_mask)),
                        ("title", Value::unicode(old_member.title), Value::unicode(member.title)),
                        ("blockRoles", Value::Bool(old_member.blocked), Value::Bool(member.blocked)),
                    ] {
                        if old != new { changes.push((Value::str(name), Value::tuple([old, new]))); }
                    }
                    if !changes.is_empty() { member_changes.push((target, Value::Dict(changes))); }
                }
                let after = crate::dao::characters::roles::authorities(tx, &ids).await.map_err(db_err)?;
                let mut changed_sources = Vec::new();
                for current in after {
                    if lines.binary_search_by_key(&current.character, |line| integer(&line[0]).unwrap_or(0)).is_err() { continue; }
                    let Some(previous) = before.get(&current.character) else { continue };
                    if previous.roles != current.roles || previous.grantable != current.grantable || previous.live != current.live {
                        changed_sources.push(current.character);
                    }
                    let has_rights = |a: &crate::dao::characters::roles::Authority| a.roles.iter().chain(a.grantable.iter()).any(|&mask| mask != 0);
                    let granted = (0..4).any(|scope| (current.roles[scope] & !previous.roles[scope]) != 0 || (current.grantable[scope] & !previous.grantable[scope]) != 0);
                    if granted {
                        role_dao::removal_clock(tx, current.character, None).await.map_err(db_err)?;
                        if let Some(mut pending) = crate::dao::corporations::pending_kicks::get(tx, bound, current.character).await.map_err(db_err)? {
                            pending.eligible_at = None;
                            crate::dao::corporations::pending_kicks::upsert(tx, &pending).await.map_err(db_err)?;
                            crate::services::corps::pending_kicks::cancel(tx, &pending).await.map_err(db_err)?;
                        }
                    }
                    else if has_rights(previous) && !has_rights(&current) {
                        role_dao::removal_clock(tx, current.character, Some(now)).await.map_err(db_err)?;
                        if let Some(mut pending) = crate::dao::corporations::pending_kicks::get(tx, bound, current.character).await.map_err(db_err)? {
                            pending.eligible_at = Some(exit::deadline(now)?);
                            crate::dao::corporations::pending_kicks::upsert(tx, &pending).await.map_err(db_err)?;
                            crate::services::corps::pending_kicks::schedule(tx, &pending).await.map_err(db_err)?;
                        }
                    }
                    if previous.roles[0] & ROLE_DIRECTOR != 0 && current.roles[0] & ROLE_DIRECTOR == 0 {
                        pending_kicks::cancel_issued(tx, bound, current.character).await.map_err(db_err)?;
                    }
                    for scope in 0..4 {
                        for (old, new, grantable) in [(previous.roles[scope], current.roles[scope], false), (previous.grantable[scope], current.grantable[scope], true)] {
                            crate::dao::corporations::audit::role(tx, current.character, bound, me, now, old, new, grantable, scope as i16).await.map_err(db_err)?;
                        }
                    }
                    if previous.roles != current.roles || previous.grantable != current.grantable {
                        let names = ["corprole", "rolesAtAll", "rolesAtHQ", "rolesAtBase", "rolesAtOther"];
                        let masks = [current.roles[0], current.roles[0], current.roles[1], current.roles[2], current.roles[3]];
                        outbox.notify_pilot(current.character, "__session", names.into_iter().zip(masks).map(|(name, value)| Value::tuple([Value::str(name), Value::Int(value)])).collect());
                    }
                }
                crate::services::sovereignty::invalidate_sources(tx, outbox, &changed_sources).await?;
                for (target, change) in member_changes {
                    for &recipient in &audience {
                        outbox.notify_pilot(recipient, "OnCorporationMemberChanged", vec![Value::Int(bound), Value::Int(target), change.clone()]);
                    }
                }
                Ok(())
            })
        },
        |value, outbox| async move { crate::scheduler::wake(); dispatch_world(value, outbox, world) },
    ).await
}

/// Execute a typed bulk action list in one locked member update transaction.
pub(super) async fn execute_actions(db: &Db, world: &World, sde: &Arc<Sde>, me: i64, bound: i64, call: &Call<'_>) -> Result<(), CallError> {
    let invalid = || notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES);
    if call.args.len() != 2 { return Err(invalid()); }
    let targets = call.args[0].items().ok_or_else(invalid)?;
    let mut lines = Vec::with_capacity(targets.len());
    for target in targets {
        let id = integer(target).filter(|id| *id > 0).ok_or_else(invalid)?;
        let mut line = vec![Value::None; 15];
        line[0] = Value::Int(id);
        lines.push(line);
    }
    let mut actions = Vec::new();
    for action in call.args[1].items().ok_or_else(invalid)? {
        let [verb, property, value] = action.items().ok_or_else(invalid)? else { return Err(invalid()) };
        let verb = integer(verb).ok_or_else(invalid)?;
        let property = property.as_text().ok_or_else(invalid)?;
        let index = match property.as_ref() {
            "roles" => 4, "grantableRoles" => 5, "rolesAtHQ" => 6, "grantableRolesAtHQ" => 7,
            "rolesAtBase" => 8, "grantableRolesAtBase" => 9, "rolesAtOther" => 10,
            "grantableRolesAtOther" => 11, "baseID" => 12, "titleMask" => 13,
            _ => return Err(invalid()),
        };
        if !matches!(verb, CTV_ADD | CTV_REMOVE | CTV_SET) || (index == 12 && verb != CTV_SET) { return Err(invalid()); }
        actions.push((index, verb, integer(value).ok_or_else(invalid)?));
    }
    update_members(db, world, sde, me, bound, lines, actions).await
}

const TITLE_HEADER: &[&str] = &[
    "titleID",
    "titleName",
    "roles",
    "grantableRoles",
    "rolesAtHQ",
    "grantableRolesAtHQ",
    "rolesAtBase",
    "grantableRolesAtBase",
    "rolesAtOther",
    "grantableRolesAtOther",
];

/// {titleID: Row} for all 16 title bits; unnamed ones the client calls "Untitled n".
pub(super) async fn titles(db: &Db, corp: i64) -> CallResult {
    let rows = role_dao::titles(db, corp).await.map_err(db_err)?;
    let header = Value::list(TITLE_HEADER.iter().map(|h| Value::str(*h)));
    Ok(Value::Dict(
        (0..TITLE_COUNT)
            .map(|bit| {
                let id = 1i64 << bit;
                let line: Vec<Value> = match rows.iter().find(|r| r.title_id == id) {
                    Some(r) => std::iter::once(Value::Int(id))
                        .chain(std::iter::once(Value::unicode(r.name.clone())))
                        .chain([r.roles, r.grantable_roles, r.roles_hq, r.grantable_hq, r.roles_base, r.grantable_base, r.roles_other, r.grantable_other].into_iter().map(Value::Int))
                        .collect(),
                    None => std::iter::once(Value::Int(id)).chain(std::iter::once(Value::unicode(""))).chain((2..10).map(|_| Value::Int(0))).collect(),
                };
                (Value::Int(id), row(header.clone(), line))
            })
            .collect(),
    ))
}

/// Update titles atomically, auditing each affected holder's effective scopes.
pub(super) async fn update_titles(db: &Db, world: &World, sde: &Arc<Sde>, me: i64, corp: i64, lines: Vec<Vec<Value>>) -> Result<(), CallError> {
    let invalid = || notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_TITLES);
    let mut titles = Vec::with_capacity(lines.len());
    for line in lines {
        if line.len() != 10 { return Err(invalid()); }
        let id = integer(&line[0]).ok_or_else(invalid)?;
        if id <= 0 || id.count_ones() != 1 || id > 1 << (TITLE_COUNT - 1) { return Err(invalid()); }
        let name = line[1].as_text().ok_or_else(invalid)?.into_owned();
        let mut masks = [0i64; 8];
        for (i, mask) in masks.iter_mut().enumerate() { *mask = integer(&line[i + 2]).ok_or_else(invalid)?; }
        if masks.iter().any(|mask| mask & ROLE_DIRECTOR != 0) { return Err(invalid()); }
        titles.push(role_dao::Title { title_id: id, name, roles: masks[0], grantable_roles: masks[1], roles_hq: masks[2], grantable_hq: masks[3], roles_base: masks[4], grantable_base: masks[5], roles_other: masks[6], grantable_other: masks[7] });
    }
    titles.sort_by_key(|title| title.title_id);
    if titles.windows(2).any(|pair| pair[0].title_id == pair[1].title_id) { return Err(invalid()); }
    mutate_titles(db, world, sde, me, corp, titles, None).await
}

pub(super) async fn delete_title(db: &Db, world: &World, sde: &Arc<Sde>, me: i64, corp: i64, title: i64) -> Result<(), CallError> {
    if title <= 0 || title.count_ones() != 1 || title > 1 << (TITLE_COUNT - 1) { return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_DELETE_TITLES)); }
    mutate_titles(db, world, sde, me, corp, Vec::new(), Some(title)).await
}

fn title_values(title: &role_dao::Title) -> [Value; 10] {
    [Value::Int(title.title_id), Value::unicode(&title.name), Value::Int(title.roles), Value::Int(title.grantable_roles),
     Value::Int(title.roles_hq), Value::Int(title.grantable_hq), Value::Int(title.roles_base), Value::Int(title.grantable_base),
     Value::Int(title.roles_other), Value::Int(title.grantable_other)]
}

async fn mutate_titles(db: &Db, world: &World, sde: &Arc<Sde>, me: i64, corp: i64, titles: Vec<role_dao::Title>, deleting: Option<i64>) -> Result<(), CallError> {
    let invalid = || notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_TITLES);
    let changed = titles.iter().fold(deleting.unwrap_or(0), |mask, title| mask | title.title_id);
    let holders = role_dao::holders(db, corp, changed, false).await.map_err(db_err)?;
    let mut locked = holders.clone();
    locked.push(me);
    let now = filetime_now();
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let titles = titles.clone();
            let holders = holders.clone();
            let locked = locked.clone();
            let sde = sde.clone();
            Box::pin(async move {
                if !registry::lock_admission(tx, &[], &[corp], &locked).await.map_err(db_err)?
                    || !permissions::has(tx, me, corp, ROLE_DIRECTOR).await?
                    || role_dao::holders(tx, corp, changed, false).await.map_err(db_err)? != holders { return Err(invalid()); }
                let old = role_dao::titles(tx, corp).await.map_err(db_err)?;
                let before: std::collections::HashMap<i64, _> = crate::dao::characters::roles::authorities(tx, &holders).await.map_err(db_err)?.into_iter().map(|row| (row.character, row)).collect();
                let old_masks = if deleting.is_some() { role_dao::holder_masks(tx, &holders).await.map_err(db_err)? } else { Vec::new() };
                let valid = sde.corporation_roles.scope_masks();
                for title in &titles {
                    let previous = old.iter().find(|row| row.title_id == title.title_id);
                    let old_masks = previous.map(|row| [row.roles,row.grantable_roles,row.roles_hq,row.grantable_hq,row.roles_base,row.grantable_base,row.roles_other,row.grantable_other]).unwrap_or([0; 8]);
                    let new_masks = [title.roles,title.grantable_roles,title.roles_hq,title.grantable_hq,title.roles_base,title.grantable_base,title.roles_other,title.grantable_other];
                    for i in 0..8 {
                        if (new_masks[i] & !old_masks[i]) & !valid[i / 2] != 0 { return Err(invalid()); }
                    }
                }
                if let Some(id) = deleting { role_dao::delete_title(tx, corp, id).await.map_err(db_err)?; }
                role_dao::save_titles(tx, corp, titles.clone()).await.map_err(db_err)?;
                let after = crate::dao::characters::roles::authorities(tx, &holders).await.map_err(db_err)?;
                let mut changed_sources = Vec::new();
                for current in after {
                    let Some(previous) = before.get(&current.character) else { continue };
                    for scope in 0..4 {
                        for (old, new, grantable) in [(previous.roles[scope], current.roles[scope], false), (previous.grantable[scope], current.grantable[scope], true)] {
                            crate::dao::corporations::audit::role(tx, current.character, corp, me, now, old, new, grantable, scope as i16).await.map_err(db_err)?;
                        }
                    }
                    if previous.roles != current.roles || previous.grantable != current.grantable {
                        changed_sources.push(current.character);
                        let before_has = previous.roles.iter().chain(previous.grantable.iter()).any(|&mask| mask != 0);
                        let after_has = current.roles.iter().chain(current.grantable.iter()).any(|&mask| mask != 0);
                        let granted = (0..4).any(|scope| current.roles[scope] & !previous.roles[scope] != 0 || current.grantable[scope] & !previous.grantable[scope] != 0);
                        if granted { role_dao::removal_clock(tx, current.character, None).await.map_err(db_err)?; }
                        else if before_has && !after_has { role_dao::removal_clock(tx, current.character, Some(now)).await.map_err(db_err)?; }
                        if let Some(mut pending) = crate::dao::corporations::pending_kicks::get(tx, corp, current.character).await.map_err(db_err)? {
                            if granted {
                                pending.eligible_at = None;
                                crate::dao::corporations::pending_kicks::upsert(tx, &pending).await.map_err(db_err)?;
                                crate::services::corps::pending_kicks::cancel(tx, &pending).await.map_err(db_err)?;
                            } else if before_has && !after_has {
                                pending.eligible_at = Some(exit::deadline(now)?);
                                crate::dao::corporations::pending_kicks::upsert(tx, &pending).await.map_err(db_err)?;
                                crate::services::corps::pending_kicks::schedule(tx, &pending).await.map_err(db_err)?;
                            }
                        }
                        if previous.roles[0] & ROLE_DIRECTOR != 0 && current.roles[0] & ROLE_DIRECTOR == 0 {
                            pending_kicks::cancel_issued(tx, corp, current.character).await.map_err(db_err)?;
                        }
                        let names = ["corprole", "rolesAtAll", "rolesAtHQ", "rolesAtBase", "rolesAtOther"];
                        let masks = [current.roles[0], current.roles[0], current.roles[1], current.roles[2], current.roles[3]];
                        outbox.notify_pilot(current.character, "__session", names.into_iter().zip(masks).map(|(name, value)| Value::tuple([Value::str(name), Value::Int(value)])).collect());
                    }
                }
                crate::services::sovereignty::invalidate_sources(tx, outbox, &changed_sources).await?;
                let audience = crate::dao::characters::members::active_ids(tx, corp).await.map_err(db_err)?;
                if let Some(id) = deleting {
                    for holder in old_masks {
                        let character = holder.character;
                        let mask = holder.mask;
                        let change = Value::dict([("titleMask", Value::tuple([Value::Int(mask), Value::Int(mask & !id)]))]);
                        for &recipient in &audience {
                            outbox.notify_pilot(recipient, "OnCorporationMemberChanged", vec![Value::Int(corp), Value::Int(character), change.clone()]);
                        }
                    }
                }
                for id in titles.iter().map(|row| row.title_id).chain(deleting) {
                    let previous = old.iter().find(|row| row.title_id == id);
                    let current = titles.iter().find(|row| row.title_id == id);
                    let previous_values = previous.map(title_values);
                    let current_values = current.map(title_values);
                    let changes = Value::Dict(TITLE_HEADER.iter().enumerate().filter_map(|(i, name)| {
                        let a = previous_values.as_ref().map(|row| row[i].clone()).unwrap_or(Value::None);
                        let b = current_values.as_ref().map(|row| row[i].clone()).unwrap_or(Value::None);
                        (a != b).then(|| (Value::str(*name), Value::tuple([a, b])))
                    }).collect());
                    for &member in &audience { outbox.notify_pilot(member, "OnTitleChanged", vec![Value::Int(corp), Value::Int(id), changes.clone()]); }
                }
                Ok(())
            })
        },
        |value, outbox| async move { crate::scheduler::wake(); dispatch_world(value, outbox, world) },
    ).await
}

/// Members other than the CEO with Corporation Management, who could take over.
pub(super) async fn potential_ceos(db: &(impl Executor + ?Sized), sde: &Sde, corp: i64, ceo: i64) -> Result<Vec<i64>, CallError> {
    let candidates = role_dao::potential_ceos(db, corp, ceo).await.map_err(db_err)?;
    if candidates.is_empty() { return Ok(Vec::new()); }
    let now = filetime_now();
    let trained = crate::dao::skills::training::current_many(db, &candidates).await.map_err(db_err)?;
    let installations = crate::dao::skills::expert_systems::active_many(db, &candidates, now).await.map_err(db_err)?;
    let mut trained_by_character = std::collections::HashMap::<i64, Vec<_>>::new();
    for (character_id, skill) in trained {
        trained_by_character.entry(character_id).or_default().push(skill);
    }
    let mut installations_by_character = std::collections::HashMap::<i64, Vec<_>>::new();
    for installation in installations {
        installations_by_character.entry(installation.character_id).or_default().push(installation);
    }
    let minimum = crate::config::game().corporations.min_management_skill_level;
    Ok(candidates.into_iter().filter(|&character_id| {
        let snapshot = crate::services::skills::effective::project(
            trained_by_character.get(&character_id).map_or(&[], Vec::as_slice),
            installations_by_character.get(&character_id).map_or(&[], Vec::as_slice),
            sde,
            now,
        );
        i64::from(snapshot.level(SKILL_CORPORATION_MANAGEMENT)) >= minimum
    }).collect())
}

/// `ResignFromCEO(newCeoID=None)`. Without a successor it returns the members who could be
/// one for the CEO to pick from; a CEO alone closes the corporation, and a CEO without
/// eligible successors closes it and sends everyone back to their school corporations.
pub(super) async fn resign_ceo(db: &Db, sde: &Arc<Sde>, world: &World, me: i64, bound: i64, successor: Option<i64>) -> CallResult {
    let (corp, _) = membership(db, me).await?;
    if corp != bound || !is_player_corp(corp) || !permissions::is_active_ceo(db, me, corp).await? {
        return Err(CallError::User { msg: messages::MSG_ONLY_CEO_CAN_RESIGN.into(), dict: vec![] });
    }
    if members(db, corp, registry::MemberFilter::Include, &[me]).await?.first().and_then(|row| row.station_id).is_none() {
        return Err(notify_error(messages::corporations::CEO_RESIGN_DOCK));
    }
    match successor {
        Some(next) => {
            if !potential_ceos(db, sde, corp, me).await?.contains(&next) {
                return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO));
            }
            let masks = sde.corporation_roles.scope_masks();
            let now = filetime_now();
            db.transaction(
                |tx, outbox: &mut Outbox| {
                    let sde = sde.clone();
                    Box::pin(async move {
                    if !registry::lock_admission(tx, &[], &[corp], &[me, next]).await.map_err(db_err)?
                        || !permissions::is_active_ceo(tx, me, corp).await?
                        || !potential_ceos(tx, &sde, corp, me).await?.contains(&next) { return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO)); }
                    if members(tx, corp, registry::MemberFilter::Include, &[me]).await?.first().and_then(|row| row.station_id).is_none() {
                        return Err(notify_error(messages::corporations::CEO_RESIGN_DOCK));
                    }
                    let before_old = permissions::load(tx, me).await?.ok_or_else(|| notify_error(messages::MEMBER_CANNOT_BECOME_CEO))?;
                    let before_next = permissions::load(tx, next).await?.ok_or_else(|| notify_error(messages::MEMBER_CANNOT_BECOME_CEO))?;
                    if before_next.blocked { return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO)); }
                    if !role_dao::transfer_ceo(tx, corp, me, next, masks).await.map_err(db_err)? { return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO)); }
                    crate::services::sovereignty::invalidate_sources(tx, outbox, &[me, next]).await?;
                    role_dao::removal_clock(tx, next, None).await.map_err(db_err)?;
                    if let Some(pending) = crate::dao::corporations::pending_kicks::get(tx, corp, next).await.map_err(db_err)? {
                        crate::dao::corporations::pending_kicks::delete(tx, corp, next).await.map_err(db_err)?;
                        pending_kicks::cancel(tx, &pending).await.map_err(db_err)?;
                    }
                    if !permissions::has(tx, me, corp, ROLE_DIRECTOR).await? {
                        pending_kicks::cancel_issued(tx, corp, me).await.map_err(db_err)?;
                    }
                    audit::event(tx, me, me, corp, audit::CEASED_CEO, now).await?;
                    audit::event(tx, next, me, corp, audit::BECAME_CEO, now).await?;
                    for (char_id, before) in [(me, before_old), (next, before_next)] {
                        let after = permissions::load(tx, char_id).await?.ok_or_else(|| notify_error(messages::MEMBER_CANNOT_BECOME_CEO))?;
                        for scope in 0..4 {
                            crate::dao::corporations::audit::role(tx, char_id, corp, me, now, before.roles[scope], after.roles[scope], false, scope as i16).await.map_err(db_err)?;
                        }
                        let names = ["corprole", "rolesAtAll", "rolesAtHQ", "rolesAtBase", "rolesAtOther"];
                        let values = [after.roles[0], after.roles[0], after.roles[1], after.roles[2], after.roles[3]];
                        outbox.notify_pilot(char_id, "__session", names.into_iter().zip(values).map(|(key, mask)| Value::tuple([Value::str(key), Value::Int(mask)])).collect());
                    }
                    let audience = crate::dao::characters::members::active_ids(tx, corp).await.map_err(db_err)?;
                    notifications::ceo(tx, outbox, &audience, corp).await?;
                    let change = Value::dict([("ceoID", Value::tuple([Value::Int(me), Value::Int(next)]))]);
                    for recipient in &audience { outbox.notify_pilot(*recipient, "OnCorporationChanged", vec![Value::Int(corp), change.clone()]); }
                    Ok(())
                    })
                },
                |value, outbox| async move { crate::scheduler::wake(); dispatch_world(value, outbox, world) },
            ).await?;
            info!("corporations: {me} handed {corp} to {next}");
            Ok(Value::None)
        }
        None => {
            let others: Vec<i64> = members(db, corp, registry::MemberFilter::Exclude, &[me]).await?.iter().map(|r| r.character_id).collect();
            let candidates = potential_ceos(db, sde, corp, me).await?;
            if !candidates.is_empty() {
                return Ok(Value::list(candidates.into_iter().map(Value::Int)));
            }
            let _market = crate::services::market::lock().await;
            let now = filetime_now();
            let sde = sde.clone();
            db.transaction(
                |tx, outbox| {
                    let others = others.clone();
                    let sde = sde.clone();
                    let mut locked = others.clone();
                    locked.push(me);
                    Box::pin(async move {
                        if !registry::lock_admission(tx, &[], &[corp], &locked).await.map_err(db_err)?
                            || !permissions::is_active_ceo(tx, me, corp).await?
                            || !potential_ceos(tx, &sde, corp, me).await?.is_empty() { return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO)); }
                        let current: Vec<i64> = members(tx, corp, registry::MemberFilter::Exclude, &[me]).await?.into_iter().map(|row| row.character_id).collect();
                        if current != others { return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO)); }
                        if members(tx, corp, registry::MemberFilter::Include, &[me]).await?.first().and_then(|row| row.station_id).is_none() {
                            return Err(notify_error(messages::corporations::CEO_RESIGN_DOCK));
                        }
                        if registry::headquarters(tx, corp).await.map_err(db_err)?.is_some_and(|row| row.alliance_id.is_some()) {
                            return Err(notify_error(messages::corporations::DISSOLVE_ALLIANCE));
                        }
                        if crate::dao::market::orders::has_open_owner_orders(tx, corp).await.map_err(db_err)? {
                            return Err(notify_error(messages::market::CORPORATION_OPEN_ORDERS));
                        }
                        let audience = crate::dao::characters::members::active_ids(tx, corp).await.map_err(db_err)?;
                        registry::close_corp(tx, corp).await.map_err(db_err)?;
                        notifications::liquidation(tx, outbox, &audience, corp).await?;
                        audit::event(tx, me, me, corp, audit::CEASED_CEO, now).await?;
                        audit::event(tx, me, me, corp, audit::DISSOLVED, now).await?;
                        for other in others {
                            let school = school_corp(tx, &sde, other).await?;
                            set_corp(tx, &sde, outbox, other, me, school, [0; 4], now).await?;
                        }
                        leave_inner(tx, &sde, outbox, me, me, now).await
                    })
                },
                |value, outbox| async move { crate::scheduler::wake(); dispatch_world(value, outbox, world) },
            ).await?;
            Ok(Value::None)
        }
    }
}

/// KeyVal(corpID, allianceID, title, title1..title16) for `char_id`: the names of the titles
/// they hold (None for the others).
pub(super) async fn info_window_data(db: &Db, char_id: i64) -> CallResult {
    let Some(r) = role_dao::info(db, char_id).await.map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    let (corp, mask) = (r.corporation_id, r.corp_title_mask);
    let names = role_dao::title_names(db, corp).await.map_err(db_err)?;
    let mut fields: Vec<(String, Value)> = vec![
        ("corpID".into(), Value::Int(corp)),
        ("allianceID".into(), r.alliance_id.map(Value::Int).unwrap_or(Value::None)),
        ("title".into(), Value::unicode(r.corp_title)),
    ];
    for bit in 0..TITLE_COUNT {
        let id = 1i64 << bit;
        let held = mask & id != 0;
        let name = names.iter().find(|n| n.title_id == id).map(|n| n.name.clone()).unwrap_or_default();
        fields.push((format!("title{}", bit + 1), if held { Value::unicode(name) } else { Value::None }));
    }
    Ok(keyval(fields))
}

