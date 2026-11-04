//! Player corporations (corpRegistry, bound per corporation; eve/client/script/ui/services/corporation).
//!
//! A character with Corporation Management can found a corporation (game.corporations, IDs
//! from 98,000,000 like the live server's) for appConst.corporationStartupCost while docked,
//! and becomes its CEO with every role. Others join by application (or invitation) that the
//! corporation's personnel managers, or the character, accept; members leave, or directors
//! kick them, back to their school's NPC corporation. Membership changes reach an online
//! pilot as a session change (corpid, corprole...), which also moves them to the new corp
//! chat channel. A member's roles are one mask (game.characters.corp_roles) used for all
//! locations. Static NPC corporations and schools come from the startup SDE. Alliances are
//! services/alliances; titles, wallet divisions, offices, shares and general votes are supported.
//! The required management skill level and initial share issuance are operator policies.

use super::{db_err, keyval, notify_error, row, rowset, trimmed_arg_text, value_id_list, Call, CallError, CallResult};
use super::alliances;
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, CORPORATION_TYPE_PLAYER, CORPORATION_UNLIMITED_MEMBER_LIMIT, CORP_DIVISION_NAME_COUNT, FILETIME_SECOND, FILETIME_UNIX_EPOCH, ORG_NAME_MAX_LENGTH, ORG_NAME_MIN_LENGTH, ORG_SHORT_NAME_MAX_LENGTH, ORG_SHORT_NAME_MIN_LENGTH, ROLE_DIRECTOR, TYPE_CHARACTER};
use crate::dao::corporations::registry;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::sde::Sde;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::sync::Arc;
use tracing::info;

/// appConst.corporationStartupCost
const STARTUP_COST: f64 = 1_599_800.0;
/// SDE invTypes: Corporation Management skill.
const SKILL_CORPORATION_MANAGEMENT: i32 = 3363;
/// Client appConst.corpRolePersonnelManager.
const ROLE_PERSONNEL_MANAGER: i64 = 128;
/// Server policy: the CEO holds every role.
const CEO_ROLES: i64 = i64::MAX;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_APPLIED: i32 = 0;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_RENEGOTIATED_BY_CHARACTER: i32 = 1;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_ACCEPTED_BY_CHARACTER: i32 = 2;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_REJECTED_BY_CHARACTER: i32 = 3;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_RENEGOTIATED_BY_CORPORATION: i32 = 5;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_ACCEPTED_BY_CORPORATION: i32 = 6;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_WITHDRAWN_BY_CHARACTER: i32 = 7;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_INVITED_BY_CORPORATION: i32 = 8;
/// Server definition of application states that remain open.
const OPEN_APPLICATION: [i32; 4] = [APP_APPLIED, APP_RENEGOTIATED_BY_CHARACTER, APP_RENEGOTIATED_BY_CORPORATION, APP_INVITED_BY_CORPORATION];
/// Client appConst.CTV_ADD.
const CTV_ADD: i64 = 1;
/// Client appConst.CTV_REMOVE.
const CTV_REMOVE: i64 = 2;
/// Client appConst.CTV_SET.
const CTV_SET: i64 = 3;
/// Client corporation title mask: sixteen title bits.
const TITLE_COUNT: usize = 16;
/// Server policy: maximum returned historical corporation applications.
const OLD_APPLICATION_LIMIT: i64 = 100;
/// Player corporation IDs (idCheckers.IsPlayerCorporation).
const FIRST_PLAYER_CORP: i64 = 98_000_000;

pub fn is_player_corp(id: i64) -> bool {
    id >= FIRST_PLAYER_CORP
}

/// The corporation `char_id` is in, and their role mask (titles included).
pub(super) async fn membership(db: &(impl Executor + ?Sized), char_id: i64) -> Result<(i64, i64), CallError> {
    let row = registry::membership(db, char_id).await.map_err(db_err)?;
    Ok((row.corporation_id, row.effective_roles))
}

/// Send an online member their current roles.
async fn refresh_roles(db: &Db, world: &World, char_id: i64) -> Result<(), CallError> {
    let (_, roles) = membership(db, char_id).await?;
    let pair = |k: &str| Value::tuple([Value::str(k), Value::Int(roles)]);
    world.notify_pilot(char_id, "__session", vec![pair("corprole"), pair("rolesAtAll"), pair("rolesAtBase"), pair("rolesAtHQ"), pair("rolesAtOther")]);
    Ok(())
}

pub(super) async fn ceo_of(db: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i64>, CallError> {
    registry::ceo_of(db, corp).await.map_err(db_err)
}

/// Move a character into `corp` with `roles`, recording the employment and updating their
/// session if they're online.
async fn set_corp<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, char_id: i64, corp: i64, roles: i64, now: i64) -> Result<(), CallError> {
    let (hq, alliance): (Option<i64>, Option<i64>) = if is_player_corp(corp) {
        registry::headquarters(db, corp).await.map_err(db_err)?
            .map(|row| (row.station_id, row.alliance_id)).unwrap_or_default()
    } else {
        (sde.npc_corporations.get(corp).and_then(|row| row.station_id), None)
    };
    registry::set_corp(db, char_id, corp, roles, now).await.map_err(db_err)?;
    let pair = |k: &str, v: Value| Value::tuple([Value::str(k), v]);
    outbox.notify_pilot(
        char_id,
        "__session",
        vec![
            pair("corpid", Value::Int(corp)),
            pair("allianceid", alliance.map(Value::Int).unwrap_or(Value::None)),
            pair("corprole", Value::Int(roles)),
            pair("rolesAtAll", Value::Int(roles)),
            pair("rolesAtBase", Value::Int(roles)),
            pair("rolesAtHQ", Value::Int(roles)),
            pair("rolesAtOther", Value::Int(roles)),
            pair("hqID", hq.map(Value::Int).unwrap_or(Value::None)),
        ],
    );
    Ok(())
}

/// The NPC corporation of the character's school.
async fn school_corp(db: &(impl Executor + ?Sized), sde: &Sde, char_id: i64) -> Result<i64, CallError> {
    let row = registry::school(db, char_id).await.map_err(db_err)?;
    row.school_id.and_then(|id| sde.characters.school(i64::from(id))).and_then(|school| school.corporation_id)
        .or_else(|| sde.characters.schools_for_race(row.race_id).next().and_then(|school| school.corporation_id))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no school corporation for {char_id}")))
}


/// Leave (or be kicked from) a player corporation, back to the school's NPC corporation.
/// A CEO can only leave as the last member, which closes the corporation.
async fn leave_inner<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, char_id: i64, now: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, char_id).await?;
    if !is_player_corp(corp) {
        return Err(notify_error(messages::CANNOT_LEAVE_NPC_CORP));
    }
    if ceo_of(db, corp).await? == Some(char_id) {
        let others = registry::other_member_count(db, corp, char_id).await.map_err(db_err)?;
        if others > 0 {
            return Err(notify_error(messages::CORP_CEO_CANNOT_LEAVE_WITH_MEMBERS));
        }
        super::alliances::leave_alliance(db, outbox, corp, now).await?;
        registry::close_corp(db, corp).await.map_err(db_err)?;
    }
    let school = school_corp(db, sde, char_id).await?;
    set_corp(db, sde, outbox, char_id, school, 0, now).await
}

async fn leave(db: &Db, sde: &Arc<Sde>, world: &World, char_id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    let sde = sde.clone();
    db.transaction(
        |tx, outbox| {
            let sde = sde.clone();
            Box::pin(async move { leave_inner(tx, &sde, outbox, char_id, now).await })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}


async fn members(db: &Db, corp: i64, filter: registry::MemberFilter, ids: &[i64]) -> Result<Vec<registry::Member>, CallError> {
    registry::members(db, corp, filter, ids, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.map_err(db_err)
}


/// Employment history rows for the character info window: (corporationID, deleted, startDate).
pub async fn employment_rows(db: &Db, char_id: i64) -> Result<Vec<Value>, CallError> {
    let rows = registry::employment(db, char_id, FILETIME_SECOND, FILETIME_UNIX_EPOCH).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| keyval([("corporationID", Value::Int(r.corporation_id)), ("deleted", Value::Bool(false)), ("startDate", Value::Int(r.start_date))]))
        .collect())
}

/// The CORP_HEADER line of a player corporation (universe::corporation).
pub async fn corporation_line(db: &Db, corp: i64) -> Result<Option<Vec<Value>>, CallError> {
    let Some(r) = registry::corporation(db, corp).await.map_err(db_err)? else {
        return Ok(None);
    };
    let issued = crate::dao::corporations::shares::issued_total(db, corp).await.map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("corporation share issuance has not been initialized")))?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    let mut line = vec![
        Value::Int(corp),
        Value::unicode(r.name),
        Value::unicode(r.description),
        Value::unicode(r.ticker),
        Value::unicode(r.url),
        Value::Float(r.tax_rate),
        Value::Float(r.lp_tax_rate),
        Value::Float(0.0),
        Value::Int(CORPORATION_TYPE_PLAYER),
        Value::Bool(true),
        Value::Bool(true),
        Value::Int(r.creator_id),
        opt(r.ceo_id),
        Value::Int(r.station_id),
        Value::None,
        opt(r.alliance_id),
        Value::None,
        Value::Int(issued),
        Value::Int(r.member_count),
        Value::Int(CORPORATION_UNLIMITED_MEMBER_LIMIT),
        Value::None,
        Value::None,
    ];
    line.extend(r.logo.into_iter().map(opt));
    // Hangar then wallet division names; None for the client's defaults.
    let divisions = r.division_names.unwrap_or_default();
    line.extend((0..CORP_DIVISION_NAME_COUNT).map(|i| divisions.get(i).cloned().flatten().filter(|n| !n.is_empty()).map(Value::unicode).unwrap_or(Value::None)));
    // allowWar represents aggressive-war eligibility; mutual wars need no permit.
    line.extend([Value::Bool(r.deleted), Value::Bool(r.is_recruiting), Value::Bool(r.friendly_fire), Value::None, Value::Int(r.created), opt(r.chosen_executor_id), Value::Bool(false)]);
    Ok(Some(line))
}


pub(crate) mod corp_registry;
pub(crate) mod shares;
mod dividends;
pub(crate) mod votes;
pub(crate) mod vote_close;
pub(crate) mod alliance_corp_registry;
mod roles;
use roles::{delete_title, execute_actions, info_window_data, potential_ceos, resign_ceo, rowset_lines, titles, update_member, update_titles};

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if let Some(result) = alliance_corp_registry::dispatch(c, call).await { return Some(result); }
    if let Some(result) = corp_registry::dispatch(c, call).await { return Some(result); }
    votes::dispatch(c, call).await
}

#[cfg(test)] mod tests;
