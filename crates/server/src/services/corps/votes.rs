//! Informational general votes for `voteManager`, mirroring the corporation voting UI.
//! Uses game corporation vote cases, options, holder snapshots, ballots and claims.
//! Sanctioned votes and actions are unsupported; results do not execute actions.

use super::*;
use infinity_core::consts::FILETIME_DAY;
use crate::dao::corporations::{shares, votes as dao};
use infinity_core::messages::corporations as text;
use crate::scheduler::{self, Job};
use crate::services::{crowset, has_role_or_is_ceo, market};

/// corporation/voting/voting.py: general vote type and status values.
const GENERAL: i64 = 4;
const OPEN: i64 = 2;
const CLOSED: i64 = 1;
const ALL: i64 = 0;
/// corporation/voting/voting.py: vote limits.
const MAX_OPEN: i64 = 1000;
const MAX_CLOSED: i64 = 20;
const MAX_DAYS: i64 = 30;
const MAX_OPTIONS: usize = 50;
/// corp_ui_votes.py: general vote form limits.
const MIN_OPTIONS: usize = 2;
const MAX_TITLE: usize = 100;
const MAX_DESCRIPTION: usize = 1000;
const MAX_OPTION: usize = 100;

fn invalid() -> CallError { notify_error(text::VOTE_INVALID) }
fn not_visible() -> CallError { notify_error(text::VOTE_NOT_VISIBLE) }

fn valid_text(value: &str, max: usize, required: bool) -> bool {
    value.chars().count() <= max && (!required || !value.trim().is_empty())
}
fn valid_case(title: &str, description: &str, options: &[String]) -> bool {
    valid_text(title, MAX_TITLE, true) && valid_text(description, MAX_DESCRIPTION, false)
        && (MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len())
        && options.iter().all(|s| valid_text(s, MAX_OPTION, true))
}

pub(super) async fn can_view(ex: &(impl Executor + ?Sized), actor: i64, issuer: i64) -> Result<bool, CallError> {
    if has_role_or_is_ceo(ex, actor, issuer, ROLE_DIRECTOR).await? { return Ok(true); }
    dao::can_view(ex, issuer, actor).await.map_err(db_err)
}

pub(super) async fn notify<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, issuer: i64, id: i64, event: &str) -> Result<(), CallError> {
    for actor in dao::observers(ex, id).await.map_err(db_err)? {
        outbox.notify_pilot(actor, event, vec![Value::Int(issuer), Value::Int(id)]);
    }
    Ok(())
}

pub(super) async fn create_inner<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, actor: i64, title: &str, description: &str, options: &[String], days: i64, now: i64) -> Result<i64, CallError> {
    if !valid_case(title, description, options) || !(1..=MAX_DAYS).contains(&days) { return Err(invalid()); }
    let ends = days.checked_mul(FILETIME_DAY).and_then(|duration| now.checked_add(duration)).ok_or_else(invalid)?;
    let (issuer, _) = membership(ex, actor).await?;
    if !is_player_corp(issuer) || !has_role_or_is_ceo(ex, actor, issuer, ROLE_DIRECTOR).await? {
        return Err(notify_error(text::VOTE_AUTHORITY_REQUIRED));
    }
    if !shares::lock_issuer(ex, issuer).await.map_err(db_err)? { return Err(notify_error(text::VOTE_AUTHORITY_REQUIRED)); }
    if !has_role_or_is_ceo(ex, actor, issuer, ROLE_DIRECTOR).await? { return Err(notify_error(text::VOTE_AUTHORITY_REQUIRED)); }
    if dao::open_count(ex, issuer, now).await.map_err(db_err)? >= MAX_OPEN { return Err(invalid()); }
    let id = dao::create(ex, issuer, actor, title, description, now, ends).await.map_err(db_err)?;
    for (index, option) in options.iter().enumerate() {
        dao::add_option(ex, id, index as i32, option).await.map_err(db_err)?;
    }
    dao::snapshot(ex, id, issuer).await.map_err(db_err)?;
    let job = Job { id: format!("{}:{id}", super::vote_close::KIND), kind: super::vote_close::KIND.into(), due_at: ends, payload: id.to_string() };
    scheduler::schedule(ex, &job).await.map_err(db_err)?;
    notify(ex, outbox, issuer, id, "OnVoteCaseChanged").await?;
    Ok(id)
}

pub(super) async fn create_case(db: &Db, world: &World, actor: i64, title: &str, description: &str, options: &[String], days: i64, now: i64) -> Result<i64, CallError> {
    let _guard = market::lock().await;
    let input = std::sync::Arc::new((title.to_owned(), description.to_owned(), options.to_vec()));
    db.transaction(|tx, outbox: &mut Outbox| {
        let input = std::sync::Arc::clone(&input);
        Box::pin(async move {
            create_inner(tx, outbox, actor, &input.0, &input.1, &input.2, days, now).await
        })
    }, |value, outbox| async move {
        let result = dispatch_world(value, outbox, world);
        scheduler::wake();
        result
    }).await
}

pub(super) async fn cast_inner<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, actor: i64, issuer: i64, id: i64, option: i32, now: i64) -> Result<(), CallError> {
    let case = dao::lock_case(ex, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
    if case.corporation_id != issuer { return Err(not_visible()); }
    if let Some(ballot) = dao::ballot(ex, id, actor).await.map_err(db_err)? {
        return if ballot.option_id == option { Ok(()) } else { Err(notify_error(text::VOTE_ALREADY_CAST)) };
    }
    if case.closed_at.is_some() || case.ends <= now { return Err(notify_error(text::VOTE_CLOSED)); }
    if !dao::options(ex, id).await.map_err(db_err)?.iter().any(|row| row.option_id == option) { return Err(invalid()); }
    let holders = dao::eligible_holders(ex, id, actor).await.map_err(db_err)?;
    if holders.is_empty() && !permissions::is_active_ceo(ex, actor, issuer).await? { return Err(notify_error(text::VOTE_NOT_ELIGIBLE)); }
    if !dao::insert_ballot(ex, id, actor, option, now).await.map_err(db_err)? { return Err(invalid()); }
    for holder in holders {
        if !dao::claim_holder(ex, id, holder.holder_id, actor).await.map_err(db_err)? {
            return Err(notify_error(text::VOTE_NOT_ELIGIBLE));
        }
    }
    notify(ex, outbox, issuer, id, "OnVoteCast").await
}

fn case_header() -> DbRowDescriptor {
    DbRowDescriptor::new([("voteCaseID", DbType::I8), ("corporationID", DbType::I4), ("characterID", DbType::I4), ("voteCaseText", DbType::WStr), ("description", DbType::WStr), ("voteType", DbType::I4), ("startDateTime", DbType::FileTime), ("endDateTime", DbType::FileTime)])
}
fn case_row(header: &DbRowDescriptor, case: &dao::Case) -> Value {
    header.row(&[Value::Int(case.id), Value::Int(case.corporation_id), Value::Int(case.creator_id), Value::unicode(&case.title), Value::unicode(&case.description), Value::Int(GENERAL), Value::Int(case.starts), Value::Int(case.ends)])
}
async fn visible_case(ex: &(impl Executor + ?Sized), actor: i64, issuer: i64, id: i64) -> Result<dao::Case, CallError> {
    let case = dao::case(ex, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
    if case.corporation_id != issuer || !can_view(ex, actor, issuer).await? { return Err(not_visible()); }
    Ok(case)
}
fn int_arg(call: &Call<'_>, index: usize) -> Result<i64, CallError> {
    match call.args.get(index) { Some(Value::Int(value)) => Ok(*value), _ => Err(invalid()) }
}
fn string_arg<'a>(call: &'a Call<'_>, index: usize) -> Result<&'a str, CallError> {
    match call.args.get(index) { Some(Value::Unicode(value)) => Ok(value), _ => Err(invalid()) }
}

async fn handle(c: &Client, call: &Call<'_>) -> CallResult {
    let actor = c.session.int("charid").unwrap_or(0);
    let db = &c.db;
    match call.method {
        "CanRunForCEO" => Ok(Value::Bool(false)),
        "ActivateSanctionedAction" => Err(notify_error(text::VOTE_UNSUPPORTED)),
        "GetCorpSanctionedActions" => {
            let header = DbRowDescriptor::new([("voteCaseID", DbType::I8), ("inEffect", DbType::I4), ("expires", DbType::FileTime)]);
            Ok(Value::reduce("carbon.common.script.sys.crowset.CIndexedRowset", vec![header.to_value(), Value::str("voteCaseID")], None))
        }
        "CanViewVotes" => Ok(Value::Bool(can_view(db, actor, int_arg(call, 0)?).await?)),
        "GetVoteCasesByCorporation" => {
            let issuer = int_arg(call, 0)?;
            if call.args.len() > 1 && ![ALL, OPEN, CLOSED].contains(&int_arg(call, 1)?) { return Err(invalid()); }
            if !can_view(db, actor, issuer).await? { return Err(not_visible()); }
            let now = filetime_now();
            let rows = dao::cases(db, issuer, now, MAX_CLOSED).await.map_err(db_err)?;
            let header = case_header();
            let (open, closed): (Vec<_>, Vec<_>) = rows.iter().partition(|r| r.closed_at.is_none() && r.ends > now);
            Ok(Value::Dict(vec![(Value::Int(OPEN), crowset(&header, open.into_iter().map(|r| case_row(&header, r)).collect())), (Value::Int(CLOSED), crowset(&header, closed.into_iter().map(|r| case_row(&header, r)).collect()))]))
        }
        "GetVoteCase" => {
            let case = visible_case(db, actor, int_arg(call, 0)?, int_arg(call, 1)?).await?;
            Ok(case_row(&case_header(), &case))
        }
        "GetVoteCaseOptions" => {
            let id = int_arg(call, 1)?;
            visible_case(db, actor, int_arg(call, 0)?, id).await?;
            let header = DbRowDescriptor::new([("optionID", DbType::I4), ("optionText", DbType::WStr), ("parameter", DbType::I8), ("parameter1", DbType::I8), ("parameter2", DbType::I8), ("votesFor", DbType::I8)]);
            let rows = dao::options(db, id).await.map_err(db_err)?.into_iter().map(|r| header.row(&[Value::Int(r.option_id.into()), Value::unicode(r.text), Value::None, Value::None, Value::None, Value::Int(r.votes_for)])).collect();
            Ok(crowset(&header, rows))
        }
        "GetVotes" => {
            let id = int_arg(call, 0)?;
            let case = dao::case(db, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
            if !can_view(db, actor, case.corporation_id).await? { return Err(not_visible()); }
            let header = DbRowDescriptor::new([("characterID", DbType::I4), ("optionID", DbType::I4), ("shares", DbType::I8)]);
            let rows = dao::ballot(db, id, actor).await.map_err(db_err)?.into_iter().map(|r| header.row(&[Value::Int(r.character_id), Value::Int(r.option_id.into()), Value::Int(r.weight)])).collect();
            Ok(crowset(&header, rows))
        }
        "InsertVoteCase" => {
            let kind = int_arg(call, 2)?;
            if kind != GENERAL { return Err(notify_error(text::VOTE_UNSUPPORTED)); }
            if call.args.len() != 5 { return Err(invalid()); }
            let title = string_arg(call, 0)?;
            let description = string_arg(call, 1)?;
            let options = match &call.args[3] { Value::List(v) | Value::Tuple(v) => v, _ => return Err(invalid()) };
            let options: Vec<String> = options.iter().map(|v| match v { Value::Unicode(s) => Ok(s.clone()), _ => Err(invalid()) }).collect::<Result<_, _>>()?;
            let id = create_case(db, &c.world, actor, title, description, &options, int_arg(call, 4)?, filetime_now()).await?;
            Ok(Value::Int(id))
        }
        "InsertVote" => {
            let issuer = int_arg(call, 0)?;
            let id = int_arg(call, 1)?;
            let option = i32::try_from(int_arg(call, 2)?).map_err(|_| invalid())?;
            db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
                // Sample the deadline only after any competing cast or close releases the case.
                dao::lock_case(tx, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
                cast_inner(tx, outbox, actor, issuer, id, option, filetime_now()).await
            }), |value, outbox| async move { dispatch_world(value, outbox, &c.world) }).await?;
            Ok(Value::None)
        }
        _ => Err(invalid()),
    }
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "voteManager" { return None; }
    match call.method {
        "CanRunForCEO" | "ActivateSanctionedAction" | "GetCorpSanctionedActions" | "CanViewVotes" | "GetVoteCasesByCorporation" | "GetVoteCase" | "GetVoteCaseOptions" | "GetVotes" | "InsertVoteCase" | "InsertVote" => Some(handle(c, call).await),
        _ => None,
    }
}
