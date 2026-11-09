//! Read-only session information, GM security-status controls and the
//! read-only queries `/where`, `/who`, `/wallet` and `/hangar`.
//! Security is stored by Crimewatch and follows its normal notification path.
//!
//! `/where` reads the live session (`solarsystemid2` is canonical while
//! docked) and names the system from the SDE; stations have no name
//! projection, so a station is reported by its real ID. `/who` lists the
//! online-pilot registry snapshot, not XMPP occupants or persisted
//! positions; the total covers the whole snapshot while only the first 50
//! rows are named in one batch. `/wallet` reports the exact character ISK
//! of the selected character and the PLEX vault of the authenticated
//! account; the caps it prints bound client display only, never the
//! stored values. `/hangar` lists the caller's direct `FLAG_HANGAR` ship
//! rows at the current station: the active ship row first, the rest in
//! item order, packed rows by exact quantity. None of them writes game
//! data, loads systems or consults targets.
use super::{db_err, slash_error, Client, CallError};
use infinity_core::consts::{CATEGORY_SHIP, CLIENT_MAX_SHORT_AMOUNT, FLAG_HANGAR, TYPE_ALLIANCE, TYPE_CHARACTER, TYPE_CORPORATION};
use infinity_core::messages;
use crate::{dao, db::outbox::dispatch_world, isk, services::crimewatch};
use crate::dao::items::loading::{GameItem, ItemQuery};
use std::collections::HashMap;

fn valid_security(value: f64) -> bool {
    value.is_finite() && (-10.0..=10.0).contains(&value)
}

fn no_args(args: &[String], usage: &str) -> Result<(), CallError> {
    if args.is_empty() { Ok(()) } else { Err(slash_error(usage.into())) }
}

fn system_name(c: &Client, system: Option<i64>) -> Option<&str> {
    system.and_then(|id| c.sde.map.system(id)).and_then(|s| s.name.as_deref())
}

/// `/where`: the caller's own location from the live session; requires a
/// selected character like the other read-only self queries.
async fn where_command(c: &Client, args: &[String]) -> Result<String, CallError> {
    no_args(args, messages::gm::GM_WHERE_USAGE)?;
    if c.session.int("charid").is_none_or(|id| id <= 0) {
        return Err(slash_error(messages::gm::GM_ARGS_NO_CHARACTER.into()));
    }
    let system = c.session.int("solarsystemid2");
    let station = c.session.int("stationid");
    Ok(messages::gm::gm_where(system, system_name(c, system), station))
}

/// `/who`: one live snapshot of the online-pilot registry, sorted outside
/// the lock; names resolve in a single batch for the visible rows only.
async fn who(c: &Client, args: &[String]) -> Result<String, CallError> {
    no_args(args, messages::gm::GM_WHO_USAGE)?;
    let mut online = c.world.online_sessions();
    online.sort_by_key(|s| s.character_id);
    let total = online.len() as i64;
    let visible = &online[..online.len().min(super::LIST_ROW_LIMIT)];
    let names = if visible.is_empty() {
        HashMap::new()
    } else {
        let ids: Vec<i64> = visible.iter().map(|s| s.character_id).collect();
        dao::characters::social::owners(&c.db, &ids, i64::from(TYPE_CHARACTER), TYPE_CORPORATION, TYPE_ALLIANCE)
            .await.map_err(db_err)?
            .into_iter()
            .filter(|o| o.type_id == i64::from(TYPE_CHARACTER))
            .map(|o| (o.id, o.name))
            .collect()
    };
    let mut text = messages::gm::gm_who_header(total);
    for session in visible {
        let name = names.get(&session.character_id).and_then(Option::as_deref);
        text.push('\n');
        text.push_str(&messages::gm::gm_who_row(
            session.character_id, name, session.system_id,
            system_name(c, session.system_id), session.station_id,
        ));
    }
    Ok(super::finish_list(text, online.len() - visible.len()))
}

/// `/wallet`: exact balances; the character ISK of the selected character
/// and the PLEX vault of the authenticated account.
async fn wallet(c: &Client, args: &[String]) -> Result<String, CallError> {
    no_args(args, messages::gm::GM_WALLET_USAGE)?;
    let char_id = c.session.int("charid").filter(|id| *id > 0)
        .ok_or_else(|| slash_error(messages::gm::GM_ARGS_NO_CHARACTER.into()))?;
    let isk = dao::wallets::characters::balance(&c.db, char_id).await.map_err(db_err)?;
    let plex = dao::accounts::plex::balance(&c.db, c.login.user_id).await.map_err(db_err)?;
    Ok(messages::gm::gm_wallet(&isk.decimal(), &plex, &isk::personal_client_cap().decimal(), CLIENT_MAX_SHORT_AMOUNT))
}

/// `/hangar`: the caller's ship rows at the current station hangar. The
/// active ship is the same qualifying row, shown first exactly once.
async fn hangar(c: &Client, args: &[String]) -> Result<String, CallError> {
    no_args(args, messages::gm::GM_HANGAR_USAGE)?;
    let Some(character) = c.session.int("charid").filter(|id| *id > 0) else {
        return Err(slash_error(messages::gm::GM_ARGS_NO_CHARACTER.into()));
    };
    let Some(station) = c.session.int("stationid") else {
        return Err(slash_error(messages::gm::GM_SHIP_DOCKED_REQUIRED.into()));
    };
    let flags = [FLAG_HANGAR];
    let mut rows = dao::items::loading::load_items(&c.db, ItemQuery::LocationOwnerFlags(station, character, &flags))
        .await.map_err(db_err)?;
    rows.retain(|item| c.sde.types.get(item.type_id).is_some_and(|t| t.category_id == Some(CATEGORY_SHIP)));
    let ships: i128 = rows.iter().map(|item| if item.singleton { 1 } else { i128::from(item.quantity) }).sum();
    let current = c.session.int("shipid").and_then(|ship| rows.iter().position(|item| item.item_id == ship));
    // The active ship is its qualifying row shown once, before the rest in
    // their original DAO order; it also occupies one of the visible rows.
    let visible: Vec<&GameItem> = current.map(|index| &rows[index])
        .into_iter()
        .chain(rows.iter().enumerate().filter_map(|(index, item)| (Some(index) != current).then_some(item)))
        .take(super::LIST_ROW_LIMIT)
        .collect();
    let names = if visible.is_empty() {
        HashMap::new()
    } else {
        let ids: Vec<i64> = visible.iter().map(|item| item.item_id).collect();
        dao::characters::social::locations(&c.db, &ids).await.map_err(db_err)?
            .into_iter()
            .map(|l| (l.id, l.name))
            .collect()
    };
    let mut text = messages::gm::gm_hangar_header(station, rows.len() as i64, ships);
    for item in &visible {
        let name = names.get(&item.item_id).and_then(Option::as_deref).filter(|s| !s.is_empty())
            .or_else(|| c.sde.types.get(item.type_id).and_then(|t| t.name.as_deref()));
        text.push('\n');
        text.push_str(&messages::gm::gm_hangar_row(item.item_id, item.type_id, name, item.quantity, item.singleton,
            Some(item.item_id) == c.session.int("shipid")));
    }
    Ok(super::finish_list(text, rows.len() - visible.len()))
}

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    if command == "session" {
        if !args.is_empty() { return Err(slash_error(messages::gm::GM_SESSION_USAGE.into())); }
        let rows = c.session.attributes().map(|(key, value)| (key, format!("{value:?}"))).collect::<Vec<_>>();
        return Ok(messages::gm::gm_session(c.session.sid, &rows));
    }
    if command != "secstatus" {
        return match command {
            "where" => where_command(c, args).await,
            "who" => who(c, args).await,
            "wallet" => wallet(c, args).await,
            "hangar" => hangar(c, args).await,
            _ => Err(slash_error(messages::gm::GM_INFO_USAGE.into())),
        };
    }
    let usage = || slash_error(messages::gm::GM_SECSTATUS_USAGE.into());
    let parsed = super::args::Args::new(args);
    let (value, target) = match args {
        [] => (None, None),
        [first] => match first.parse::<f64>() {
            Ok(value) => (Some(value), None),
            Err(_) => (None, Some(first.as_str())),
        },
        [_, target] => (Some(parsed.f64(0).map_err(|_| usage())?), Some(target.as_str())),
        _ => return Err(usage()),
    };
    if value.is_some_and(|v| !valid_security(v)) { return Err(usage()); }
    let char_id = match target {
        Some(_) => {
            parsed.required(args.len() - 1).map_err(|_| usage())?;
            parsed.target(c, args.len() - 1).await?
        }
        None => c.session.int("charid").ok_or_else(usage)?,
    };
    let status = if let Some(value) = value {
        let world = &*c.world;
        c.db.transaction(
            |tx, outbox| Box::pin(async move { crimewatch::set_security(tx, outbox, char_id, value).await }),
            |status, outbox| async move { dispatch_world(status, outbox, world) },
        ).await?
    } else {
        crimewatch::security_status(&c.db, char_id).await?
    };
    Ok(messages::gm::gm_secstatus(char_id, status))
}

#[cfg(test)]
mod tests;
