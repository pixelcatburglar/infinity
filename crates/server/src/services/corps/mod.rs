//! Player corporations (corpRegistry, bound per corporation; eve/client/script/ui/services/corporation).
//!
//! A character with Corporation Management can found a corporation (game.corporations, IDs
//! from 98,000,000 like the live server's) for appConst.corporationStartupCost while docked,
//! and becomes its CEO with every role. Others join by application (or invitation) that the
//! corporation's personnel managers, or the character, accept; members leave, or directors
//! kick them, back to their school's NPC corporation. Membership changes reach an online
//! pilot as a session change (corpid, corprole...), which also moves them to the new corp
//! chat channel. Confirmed admissions deliver the configured nonempty welcome body as automated
//! mail in the membership transaction; edits invalidate online members' caches after commit.
//! Application queries require current personnel authority; committed application changes
//! notify the applicant and current managers, with durable application notices. Membership
//! changes also invalidate affected member and player-corporation counts after commit.
//! Members have global, HQ, base and other role scopes, including title grants; delayed
//! kick intents use the shared exit policy and durable scheduler.
//! Static NPC corporations and schools come from the startup SDE. Alliances are
//! services/alliances; titles, wallet divisions, offices, shares and general votes are supported.
//! The required management skill level and initial share issuance are operator policies.

use super::{db_err, keyval, notify_error, row, rowset, trimmed_arg_text, value_id_list, Call, CallError, CallResult};
use super::alliances;
use crate::client::{filetime_now, Client};
use infinity_core::consts::{ACCOUNT_CASH, CORPORATION_TYPE_PLAYER, CORP_DIVISION_NAME_COUNT, FILETIME_SECOND, FILETIME_UNIX_EPOCH, ORG_NAME_MAX_LENGTH, ORG_NAME_MIN_LENGTH, ORG_SHORT_NAME_MAX_LENGTH, ORG_SHORT_NAME_MIN_LENGTH, ROLE_DIRECTOR, TYPE_CHARACTER};
use crate::dao::corporations::registry;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use infinity_core::messages;
use crate::sde::Sde;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::sync::Arc;
use tracing::info;

/// appConst.corporationStartupCost
const STARTUP_COST: i128 = 1_599_800;
/// SDE invTypes: Corporation Management skill.
const SKILL_CORPORATION_MANAGEMENT: i32 = 3363;
/// Client appConst.corpRolePersonnelManager.
const ROLE_PERSONNEL_MANAGER: i64 = 128;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_APPLIED: i32 = 0;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_ACCEPTED_BY_CHARACTER: i32 = 2;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_REJECTED_BY_CHARACTER: i32 = 3;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_REJECTED_BY_CORPORATION: i32 = 4;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_ACCEPTED_BY_CORPORATION: i32 = 6;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_WITHDRAWN_BY_CHARACTER: i32 = 7;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_INVITED_BY_CORPORATION: i32 = 8;
/// Server definition of application states that remain open.
const OPEN_APPLICATION: [i32; 3] = [APP_APPLIED, APP_ACCEPTED_BY_CORPORATION, APP_INVITED_BY_CORPORATION];
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

/// Migrate legacy folded masks exactly once before serving corporation operations.
pub(crate) async fn initialize(db: &Db, sde: &Sde) -> Result<(), CallError> {
    let local = sde.corporation_roles.local;
    db.transaction(
        |tx, _: &mut Outbox| Box::pin(async move { crate::dao::corporations::roles::migrate_scopes(tx, local).await.map_err(db_err) }),
        |(), _| async { Ok(()) },
    ).await?;
    db.transaction(
        |tx, _: &mut Outbox| Box::pin(async move {
            let rows = crate::dao::corporations::pending_kicks::all(tx).await.map_err(db_err)?;
            let corps: Vec<_> = rows.iter().map(|row| row.corporation_id).collect();
            let characters: Vec<_> = rows.iter().flat_map(|row| [row.character_id, row.issuer_id]).collect();
            if !registry::lock_admission(tx, &[], &corps, &characters).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during pending kick recovery")));
            }
            let now = filetime_now();
            for original in rows {
                let Some(mut row) = crate::dao::corporations::pending_kicks::get(tx, original.corporation_id, original.character_id).await.map_err(db_err)? else { continue };
                if row.requested_at != original.requested_at || row.issuer_id != original.issuer_id { continue; }
                match exit::admit(tx, None, row.character_id, row.issuer_id, row.corporation_id, false, now).await? {
                    exit::Admission::Denied => {
                        crate::dao::corporations::pending_kicks::delete(tx, row.corporation_id, row.character_id).await.map_err(db_err)?;
                        pending_kicks::cancel(tx, &row).await.map_err(db_err)?;
                        continue;
                    }
                    exit::Admission::AwaitRoleRemoval => row.eligible_at = None,
                    exit::Admission::WaitUntil(deadline) => row.eligible_at = Some(deadline),
                    exit::Admission::Ready => row.eligible_at = Some(now),
                    exit::Admission::AwaitSafePresence => unreachable!("startup has no online pilots"),
                }
                crate::dao::corporations::pending_kicks::upsert(tx, &row).await.map_err(db_err)?;
                if row.eligible_at.is_some() { pending_kicks::schedule(tx, &row).await.map_err(db_err)?; }
                else { pending_kicks::cancel(tx, &row).await.map_err(db_err)?; }
            }
            Ok(())
        }),
        |(), _| async { crate::scheduler::wake(); Ok(()) },
    ).await?;
    Ok(())
}
pub fn is_player_corp(id: i64) -> bool {
    id >= FIRST_PLAYER_CORP
}

/// The corporation `char_id` is in, and their role mask (titles included).
pub(super) async fn membership(db: &(impl Executor + ?Sized), char_id: i64) -> Result<(i64, i64), CallError> {
    let row = registry::membership(db, char_id).await.map_err(db_err)?;
    Ok((row.corporation_id, row.effective_roles))
}


pub(super) async fn ceo_of(db: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i64>, CallError> {
    registry::ceo_of(db, corp).await.map_err(db_err)
}

/// Move a character into `corp` with `roles`, recording the employment and updating their
/// session if they're online.
async fn set_corp<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, char_id: i64, issuer: i64, corp: i64, roles: [i64; 4], now: i64) -> Result<bool, CallError> {
    let old = membership(db, char_id).await?.0;
    if old == corp { return Ok(false); }
    if is_player_corp(old) {
        pending_kicks::cancel_issued(db, old, char_id).await.map_err(db_err)?;
    }
    let old_members = crate::dao::characters::members::active_ids(db, old).await.map_err(db_err)?;
    let new_members = crate::dao::characters::members::active_ids(db, corp).await.map_err(db_err)?;
    let (hq, alliance): (Option<i64>, Option<i64>) = if is_player_corp(corp) {
        registry::headquarters(db, corp).await.map_err(db_err)?
            .map(|row| (row.station_id, row.alliance_id)).unwrap_or_default()
    } else {
        (sde.npc_corporations.get(corp).and_then(|row| row.station_id), None)
    };
    if !registry::set_corp(db, char_id, corp, roles, now).await.map_err(db_err)? {
        return Ok(false);
    }
    crate::services::sovereignty::invalidate_sources(db, outbox, &[char_id]).await?;
    if is_player_corp(old) && registry::corporation(db, old).await.map_err(db_err)?.is_some_and(|corporation| !corporation.deleted) && issuer == char_id {
        notifications::voluntary(db, outbox, char_id, old).await?;
    }
    if is_player_corp(old) {
        if let Some(pending) = crate::dao::corporations::pending_kicks::get(db, old, char_id).await.map_err(db_err)? {
            crate::dao::corporations::pending_kicks::delete(db, old, char_id).await.map_err(db_err)?;
            pending_kicks::cancel(db, &pending).await.map_err(db_err)?;
        }
    }
    if is_player_corp(old) {
        audit::event(db, char_id, issuer, old, audit::LEFT, now).await?;
    }
    if is_player_corp(corp) {
        audit::event(db, char_id, issuer, corp, audit::JOINED, now).await?;
    }
    let authority = permissions::load(db, char_id).await?.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing transitioned corporation member")))?;
    let pair = |k: &str, v: Value| Value::tuple([Value::str(k), v]);
    outbox.notify_pilot(
        char_id,
        "__session",
        vec![
            pair("corpid", Value::Int(corp)),
            pair("allianceid", alliance.map(Value::Int).unwrap_or(Value::None)),
            pair("corprole", Value::Int(authority.roles[0])),
            pair("rolesAtAll", Value::Int(authority.roles[0])),
            pair("rolesAtBase", Value::Int(authority.roles[2])),
            pair("rolesAtHQ", Value::Int(authority.roles[1])),
            pair("rolesAtOther", Value::Int(authority.roles[3])),
            pair("corpAccountKey", Value::Int(ACCOUNT_CASH as i64)),
            pair("hqID", hq.map(Value::Int).unwrap_or(Value::None)),
        ],
    );
    let change = Value::dict([("corporationID", Value::tuple([Value::Int(old), Value::Int(corp)]))]);
    for (audience, members) in [(old, &old_members), (corp, &new_members)] {
        for &member in members {
            if member != char_id {
                outbox.notify_pilot(member, "OnCorporationMemberChanged", vec![Value::Int(audience), Value::Int(char_id), change.clone()]);
            }
        }
    }
    outbox.notify_pilot(char_id, "OnCorporationMemberChanged", vec![Value::Int(corp), Value::Int(char_id), change]);
    for (audience, members, delta) in [(old, &old_members, -1), (corp, &new_members, 1)] {
        if is_player_corp(audience) {
            let before = members.len() as i64;
            let args = vec![Value::Int(audience), Value::dict([("memberCount", Value::tuple([Value::Int(before), Value::Int(before + delta)]))])];
            for &member in members {
                if member != char_id { outbox.notify_pilot(member, "OnCorporationChanged", args.clone()); }
            }
            if audience == corp { outbox.notify_pilot(char_id, "OnCorporationChanged", args); }
        }
    }
    Ok(true)
}

/// The NPC corporation of the character's school.
async fn school_corp(db: &(impl Executor + ?Sized), sde: &Sde, char_id: i64) -> Result<i64, CallError> {
    let row = registry::school(db, char_id).await.map_err(db_err)?;
    row.school_id.and_then(|id| sde.characters.school(i64::from(id))).and_then(|school| school.corporation_id)
        .or_else(|| sde.characters.schools_for_race(row.race_id).next().and_then(|school| school.corporation_id))
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("no school corporation for {char_id}")))
}


pub(super) async fn leave_inner<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, char_id: i64, issuer: i64, now: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, char_id).await?;
    if !is_player_corp(corp) {
        return Err(notify_error(messages::CANNOT_LEAVE_NPC_CORP));
    }
    if issuer != char_id {
        notifications::kicked(db, outbox, char_id, corp).await?;
    }
    let school = school_corp(db, sde, char_id).await?;
    set_corp(db, sde, outbox, char_id, issuer, school, [0; 4], now).await.map(|_| ())
}


async fn members(db: &(impl Executor + ?Sized), corp: i64, filter: registry::MemberFilter, ids: &[i64]) -> Result<Vec<registry::Member>, CallError> {
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
pub async fn corporation_line(db: &Db, sde: &Sde, corp: i64) -> Result<Option<Vec<Value>>, CallError> {
    let Some(r) = registry::corporation(db, corp).await.map_err(db_err)? else {
        return Ok(None);
    };
    let member_limit = match r.member_limit {
        Some(limit) => limit,
        None => abilities::capacity(db, sde, r.ceo_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("legacy corporation has no CEO for capacity snapshot")))?, filetime_now()).await?,
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
        Value::Int(member_limit),
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
pub(crate) mod permissions;
pub(crate) mod audit;
pub(crate) mod exit;
pub(crate) mod pending_kicks;
pub(crate) mod tax;
mod notifications;
mod abilities;
pub(crate) mod shares;
mod dividends;
pub(crate) mod votes;
pub(crate) mod vote_close;
pub(crate) mod alliance_corp_registry;
mod roles;
mod members;
mod profile;
use roles::{delete_title, execute_actions, info_window_data, potential_ceos, resign_ceo, title_rowset, titles, update_member, update_titles};

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if let Some(result) = alliance_corp_registry::dispatch(c, call).await { return Some(result); }
    if let Some(result) = corp_registry::dispatch(c, call).await { return Some(result); }
    votes::dispatch(c, call).await
}

#[cfg(test)] mod tests;
