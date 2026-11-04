//! Owner and war-opponent lookup for `lookupSvc`, used by corporation invitations,
//! `searchOld.py`, `pcownerpickerdialog.py` and `war_cso.py`.
//! Searches active game owners and startup SDE NPC owners; structures and aggressive
//! war permits are not supported.

use crate::{client::Client, consts::{TYPE_ALLIANCE, TYPE_CHARACTER, TYPE_CORPORATION, TYPE_FACTION}, dao, messages};
use crate::services::{Call, CallResult, db_err, keyval, notify_error};
use marshal::Value;
use std::collections::HashSet;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "lookupSvc" { return None; }
    match call.method {
        "LookupWarableCorporationsOrAlliances" => Some(war_lookup(c, call).await),
        "LookupEvePlayerCharacters" | "LookupOwners" | "LookupPCOwners" => Some(owner_lookup(c, call).await),
        _ => None,
    }
}

async fn war_lookup(c: &Client, call: &Call<'_>) -> CallResult {
    if call.args.len() != 2 { return Err(notify_error(messages::wars::INVALID)); }
    let text = call.args.first().and_then(Value::as_text).ok_or_else(|| notify_error(messages::wars::INVALID))?;
    let exact = match call.args.get(1) {
        Some(Value::Bool(value)) => *value,
        _ => return Err(notify_error(messages::wars::INVALID)),
    };
    let text = text.trim();
    if text.is_empty() { return Err(notify_error(messages::wars::INVALID)); }
    let corps = dao::corporations::wars::search(&c.db, text, exact).await.map_err(db_err)?;
    let alliances = dao::alliances::wars::search(&c.db, text, exact).await.map_err(db_err)?;
    let row = |id, name: String, type_id| keyval(vec![
        ("ownerID", Value::Int(id)), ("name", Value::unicode(name)),
        ("typeID", Value::Int(type_id)), ("warPermit", Value::Bool(false)),
    ]);
    Ok(Value::list(corps.into_iter().map(|r| row(r.id, r.name, TYPE_CORPORATION))
        .chain(alliances.into_iter().map(|r| row(r.id, r.name, TYPE_ALLIANCE)))))
}

async fn owner_lookup(c: &Client, call: &Call<'_>) -> CallResult {
    if call.args.len() != 2 { return Err(notify_error(messages::lookup::INVALID)); }
    let text = call.args[0].as_text().ok_or_else(|| notify_error(messages::lookup::INVALID))?;
    let text = text.trim();
    let exact = match &call.args[1] {
        Value::Bool(value) => *value,
        Value::Int(value) => *value != 0,
        Value::BigLong(value) => value.iter().any(|byte| *byte != 0),
        _ => return Err(notify_error(messages::lookup::INVALID)),
    };
    if text.is_empty() { return Ok(Value::list([])); }
    let characters = dao::characters::lookup::search(&c.db, text, exact).await.map_err(db_err)?;
    if call.method == "LookupEvePlayerCharacters" {
        return Ok(Value::list(characters.into_iter().map(|r| keyval(vec![("characterID", Value::Int(r.id))]))));
    }
    let mut owners: Vec<(i64, String, i64)> = characters.into_iter().map(|r| (r.id, r.name, i64::from(TYPE_CHARACTER))).collect();
    owners.extend(dao::corporations::wars::search(&c.db, text, exact).await.map_err(db_err)?.into_iter().map(|r| (r.id, r.name, TYPE_CORPORATION)));
    owners.extend(dao::alliances::wars::search(&c.db, text, exact).await.map_err(db_err)?.into_iter().map(|r| (r.id, r.name, TYPE_ALLIANCE)));
    if call.method == "LookupOwners" {
        let needle = text.to_lowercase();
        let matches = |name: &str| { let name = name.to_lowercase(); if exact { name == needle } else { name.contains(&needle) } };
        for r in c.sde.characters.npc_characters() {
            let name = r.name_id.and_then(|id| c.sde.localization.chinese(id)).or(r.name.as_deref());
            if let Some(name) = name.filter(|name| matches(name)) { owners.push((r.character_id, name.to_owned(), i64::from(TYPE_CHARACTER))); }
        }
        for r in c.sde.npc_corporations.iter() {
            if let Some(name) = r.name.as_deref().filter(|name| matches(name)) { owners.push((r.corporation_id, name.to_owned(), TYPE_CORPORATION)); }
        }
        for r in c.sde.factions.iter() {
            if let Some(name) = r.name.as_deref().filter(|name| matches(name)) { owners.push((r.faction_id, name.to_owned(), TYPE_FACTION)); }
        }
    }
    owners.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)).then(a.2.cmp(&b.2)));
    let mut seen = HashSet::new();
    Ok(Value::list(owners.into_iter().filter(|(id, _, _)| seen.insert(*id)).filter_map(|(id, name, type_id)| {
        let group = c.sde.types.get(i32::try_from(type_id).ok()?)?.group_id?;
        Some(keyval(vec![("ownerID", Value::Int(id)), ("ownerName", Value::unicode(name)), ("typeID", Value::Int(type_id)), ("groupID", Value::Int(i64::from(group)))]))
    })))
}

#[cfg(test)] mod tests;
