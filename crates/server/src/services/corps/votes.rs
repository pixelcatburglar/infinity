//! Corporation votes for `voteManager`, mirroring the corporation voting UI
//! (voting.py and the shared neocom corporation panels). Six non-war vote types
//! create cases over the game corp vote tables with holder snapshots, ballots and
//! claims; decided cases produce sanctioned actions (services/corps/sanctions.rs).
//! War votes stay unsupported.
//! Cast admission locks every corporation whose shares the actor may proxy
//! before the case lock; the ballot deadline is sampled after those locks.

use super::*;
use crate::dao::corporations::{registry, sanctions as sanction_dao, votes as dao};
use infinity_core::messages::corporations as text;
use crate::services::crowset;
use std::sync::Arc;

/// corporation/voting/voting.py: case status filter values.
const OPEN: i64 = 2;
const CLOSED: i64 = 1;
const ALL: i64 = 0;
/// corporation/voting/voting.py: vote limits.
pub(super) const MAX_OPEN: i64 = 1000;
const MAX_CLOSED: i64 = 20;

pub(super) fn invalid() -> CallError { notify_error(text::VOTE_INVALID) }
fn not_visible() -> CallError { notify_error(text::VOTE_NOT_VISIBLE) }

pub(super) async fn can_view(ex: &(impl Executor + ?Sized), actor: i64, issuer: i64) -> Result<bool, CallError> {
    let Some(authority) = permissions::load(ex, actor).await? else { return Ok(false) };
    if authority.live && authority.corporation == issuer && authority.roles[0] & ROLE_DIRECTOR != 0 {
        return Ok(true);
    }
    // A sitting CEO frozen by this corporation's own active election keeps the
    // view-only path even with no shares yet claimed; no other inactive CEO.
    if authority.live && authority.corporation == issuer && authority.ceo == Some(actor)
        && dao::election(ex, issuer).await.map_err(db_err)?.is_some_and(|case| {
            case.previous_ceo_id == Some(actor) && case.previous_ceo_was_active == Some(true)
        }) {
        return Ok(true);
    }
    dao::can_view(ex, issuer, actor).await.map_err(db_err)
}
pub(super) async fn notify<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, issuer: i64, id: i64, event: &str) -> Result<(), CallError> {
    for actor in dao::observers(ex, id).await.map_err(db_err)? {
        outbox.notify_pilot(actor, event, vec![Value::Int(issuer), Value::Int(id)]);
    }
    Ok(())
}

/// Cast one ballot inside the caller's transaction. The caller already holds the
/// admission locks; the case lock taken here comes after them, and every snapshot
/// holder may be claimed by one ballot only. Replaying the same option succeeds
/// without another claim.
pub(super) async fn cast_inner<X: Send>(ex: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, actor: i64, issuer: i64, id: i64, option: i32, now: i64) -> Result<(), CallError> {
    let case = dao::lock_case(ex, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
    if case.corporation_id != issuer { return Err(not_visible()); }
    if let Some(ballot) = dao::ballot(ex, id, actor).await.map_err(db_err)? {
        return if ballot.option_id == option { Ok(()) } else { Err(notify_error(text::VOTE_ALREADY_CAST)) };
    }
    if case.closed_at.is_some() || case.ends <= now { return Err(notify_error(text::VOTE_CLOSED)); }
    if !dao::options(ex, id).await.map_err(db_err)?.iter().any(|row| row.option_id == option) { return Err(invalid()); }
    let holders = dao::eligible_holders(ex, id, actor).await.map_err(db_err)?;
    // A sitting CEO casts a weightless ballot; the CEO frozen by this very
    // election keeps the same view-only path, while other inactive CEOs do not.
    let ceo_ballot = permissions::is_active_ceo(ex, actor, issuer).await?
        || (case.vote_type == vote_request::VOTE_CEO
            && case.previous_ceo_id == Some(actor) && case.previous_ceo_was_active == Some(true));
    if holders.is_empty() && !ceo_ballot { return Err(notify_error(text::VOTE_NOT_ELIGIBLE)); }
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
    header.row(&[Value::Int(case.id), Value::Int(case.corporation_id), Value::Int(case.creator_id), Value::unicode(&case.title), Value::unicode(&case.description), Value::Int(case.vote_type), Value::Int(case.starts), Value::Int(case.ends)])
}
/// corp_ui_sanctionableactions.py sanctioned action row; `timeRescended` keeps the
/// client's spelling even though the column is `time_rescinded`.
fn action_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("voteCaseID", DbType::I8), ("voteType", DbType::I4), ("optionID", DbType::I4),
        ("parameter", DbType::I8), ("parameter1", DbType::I8), ("parameter2", DbType::I8),
        ("expires", DbType::FileTime), ("actedUpon", DbType::I4), ("inEffect", DbType::I4),
        ("timeActedUpon", DbType::FileTime), ("timeRescended", DbType::FileTime),
    ])
}
fn option_value(value: Option<i64>) -> Value {
    value.map(Value::Int).unwrap_or(Value::None)
}
fn action_row(header: &DbRowDescriptor, row: &sanction_dao::ActionRow) -> Value {
    header.row(&[
        Value::Int(row.action.case_id), Value::Int(row.vote_type), Value::Int(row.action.option_id.into()),
        option_value(row.parameter), option_value(row.parameter1), option_value(row.parameter2),
        Value::Int(row.action.expires), Value::Int(row.action.acted_upon.into()), Value::Int(row.action.in_effect.into()),
        option_value(row.action.time_acted_upon), option_value(row.action.time_rescinded),
    ])
}
async fn visible_case(ex: &(impl Executor + ?Sized), actor: i64, issuer: i64, id: i64) -> Result<dao::Case, CallError> {
    let case = dao::case(ex, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
    if case.corporation_id != issuer || !can_view(ex, actor, issuer).await? { return Err(not_visible()); }
    Ok(case)
}
fn int_arg(call: &Call<'_>, index: usize) -> Result<i64, CallError> {
    match call.args.get(index) { Some(Value::Int(value)) => Ok(*value), _ => Err(invalid()) }
}

async fn handle(c: &Client, call: &Call<'_>) -> CallResult {
    let actor = c.session.int("charid").unwrap_or(0);
    let db = &c.db;
    match call.method {
        "CanRunForCEO" => {
            if !call.args.is_empty() { return Err(invalid()); }
            let (issuer, _) = membership(db, actor).await?;
            Ok(Value::Bool(vote_request::can_run_for_ceo(db, &c.sde, actor, issuer, filetime_now()).await?))
        }
        "ActivateSanctionedAction" => {
            let id = int_arg(call, 0)?;
            sanctions::activate(&crate::services::identity::Identity::of_client(c), db, &c.world, &c.sde, id).await?;
            Ok(Value::None)
        }
        "GetCorpSanctionedActions" => {
            if !call.args.is_empty() { return Err(invalid()); }
            let (issuer, _) = membership(db, actor).await?;
            if !can_view(db, actor, issuer).await? { return Err(not_visible()); }
            let header = action_header();
            let rows = sanction_dao::list(db, issuer).await.map_err(db_err)?;
            Ok(Value::Reduce {
                data: Box::new(Value::tuple([Value::global("carbon.common.script.sys.crowset.CIndexedRowset"), Value::tuple([header.to_value(), Value::str("voteCaseID")])])),
                list: vec![],
                dict: rows.iter().map(|row| (Value::Int(row.action.case_id), action_row(&header, row))).collect(),
            })
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
            let rows = dao::options(db, id).await.map_err(db_err)?.into_iter().map(|r| header.row(&[Value::Int(r.option_id.into()), Value::unicode(r.text), option_value(r.parameter), option_value(r.parameter1), option_value(r.parameter2), Value::Int(r.votes_for)])).collect();
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
            let request = Arc::new(vote_request::parse(call, actor)?);
            let id = vote_request::create(db, &c.world, &c.sde, actor, request).await?;
            Ok(Value::Int(id))
        }
        "InsertVote" => {
            let issuer = int_arg(call, 0)?;
            let id = int_arg(call, 1)?;
            let option = i32::try_from(int_arg(call, 2)?).map_err(|_| invalid())?;
            db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
                // Admission before the case: lock every corporation whose shares the
                // actor may proxy as CEO plus the issuer, then the actor, and re-verify
                // the proxy set has not drifted before sampling the ballot deadline.
                let case = dao::case(tx, id).await.map_err(db_err)?.ok_or_else(not_visible)?;
                if case.corporation_id != issuer { return Err(not_visible()); }
                let proxied = dao::proxied_corporations(tx, actor).await.map_err(db_err)?;
                let mut corps = proxied.clone();
                if !corps.contains(&issuer) { corps.push(issuer); }
                if !registry::lock_admission(tx, &[], &corps, &[actor]).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during vote admission")));
                }
                if dao::proxied_corporations(tx, actor).await.map_err(db_err)? != proxied {
                    return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during vote admission")));
                }
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
