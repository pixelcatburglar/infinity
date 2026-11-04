//! Alliances (allianceRegistry, bound per alliance; eve/client/script/ui/services/alliances).
//!
//! A player corporation's CEO founds an alliance (game.alliances, IDs from 99,000,000 like
//! the live server's) for appConst.allianceCreationCost from the corporation's master wallet;
//! the corporation becomes its executor. Other corporations'
//! CEOs apply, and the executor's directors accept (the corporation joins at once) or reject.
//! Admission commits membership, application transitions and history together, then
//! delivers session changes before alliance row notifications.
//! A member corporation leaves, or the executor's directors remove it; when the executor
//! leaves, the largest remaining member takes over, and the last member leaving closes the
//! alliance. Members back an executor (DeclareExecutorSupport); a candidate backed by
//! corporations holding more than half the alliance's pilots takes over. Membership changes
//! reach every online member as a session change (allianceid) and as the client's
//! OnAlliance*Changed row updates. Mutual wars are supported separately; bills,
//! sovereignty, capital systems and prime time aren't supported.

use super::corps::membership;
use crate::dao::alliances as dao;
use super::{crowset, db_err, notify_error, trimmed_arg_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::ROLE_DIRECTOR;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};

pub(crate) mod alliance_registry;
pub(crate) mod admission;

/// Client appConst.allianceApplicationNew.
pub(crate) const APP_NEW: i32 = 1;
/// Client appConst.allianceApplicationAccepted.
pub(crate) const APP_ACCEPTED: i32 = 2;
/// Client appConst.allianceApplicationEffective.
pub(crate) const APP_EFFECTIVE: i32 = 3;
/// Client appConst.allianceApplicationRejected.
pub(crate) const APP_REJECTED: i32 = 4;
/// Server policy: default number of ranked alliances returned.
const DEFAULT_RANKED_LIMIT: i64 = 100;
/// Server policy: maximum number of ranked alliances returned.
const MAX_RANKED_LIMIT: i64 = 1000;
/// Server majority calculation: twice the backing must exceed the total.
const MAJORITY_DENOMINATOR: i64 = 2;
/// Server suggestion algorithm: maximum ticker or short-name length.
const SUGGESTION_MAX_LENGTH: usize = 5;
/// Server suggestion algorithm: fallback compact-name lengths.
const SUGGESTION_FALLBACK_LENGTHS: [usize; 2] = [4, 3];
/// Server suggestion algorithm: minimum returned name length.
const SUGGESTION_MIN_LENGTH: usize = 2;

/// A director of the alliance's executor corporation.
pub(super) async fn executor_director(db: &(impl Executor + ?Sized), me: i64, alliance: i64) -> Result<bool, CallError> {
    let (corp, roles) = membership(db, me).await?;
    let executor = dao::executor_corp(db, alliance).await.map_err(db_err)?;
    Ok(executor == Some(corp) && roles & ROLE_DIRECTOR != 0)
}

/// `dbutil.CIndexedRowset(header, column)` filled with `rows` keyed by their `column` value.
pub(crate) fn indexed_rowset(header: &DbRowDescriptor, column: &str, rows: Vec<(i64, Value)>) -> Value {
    Value::Reduce {
        data: Box::new(Value::tuple([
            Value::global("carbon.common.script.sys.crowset.CIndexedRowset"),
            Value::tuple([header.to_value(), Value::str(column)]),
        ])),
        list: vec![],
        dict: rows.into_iter().map(|(k, row)| (Value::Int(k), row)).collect(),
    }
}

/// An OnAlliance*Changed `change`: {column: (old, new)}; a missing side is an added or
/// removed row.
fn change(columns: &[&str], old: Option<&[Value]>, new: Option<&[Value]>) -> Value {
    let side = |v: Option<&[Value]>, i: usize| v.and_then(|v| v.get(i)).cloned().unwrap_or(Value::None);
    Value::dict(columns.iter().enumerate().map(|(i, c)| (*c, Value::tuple([side(old, i), side(new, i)]))))
}

// ---------------------------------------------------------------- rows

const ALLIANCE_COLUMNS: &[&str] = &[
    "allianceID",
    "allianceName",
    "shortName",
    "executorCorpID",
    "creatorCorpID",
    "creatorCharID",
    "warFactionID",
    "description",
    "url",
    "startDate",
    "memberCount",
    "corporationCount",
    "dictatorial",
    "allowWar",
    "currentCapital",
    "deleted",
];

fn alliance_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("allianceID", DbType::I4),
        ("allianceName", DbType::WStr),
        ("shortName", DbType::WStr),
        ("executorCorpID", DbType::I4),
        ("creatorCorpID", DbType::I4),
        ("creatorCharID", DbType::I4),
        ("warFactionID", DbType::I4),
        ("description", DbType::WStr),
        ("url", DbType::WStr),
        ("startDate", DbType::FileTime),
        ("memberCount", DbType::I4),
        ("corporationCount", DbType::I4),
        ("dictatorial", DbType::Bool),
        ("allowWar", DbType::Bool),
        ("currentCapital", DbType::I4),
        ("deleted", DbType::Bool),
    ])
}


fn alliance_line(r: dao::Alliance) -> Vec<Value> {
    vec![
        Value::Int(r.id),
        Value::unicode(r.name),
        Value::unicode(r.short_name),
        r.executor.map(Value::Int).unwrap_or(Value::None),
        Value::Int(r.creator_corp),
        Value::Int(r.creator_char),
        Value::None,
        Value::unicode(r.description),
        Value::unicode(r.url),
        Value::Int(r.created),
        Value::Int(r.pilots),
        Value::Int(r.corporations),
        Value::Bool(r.dictatorial),
        Value::Bool(true),
        Value::None,
        Value::Bool(r.deleted),
    ]
}

async fn alliance_line_of(db: &(impl Executor + ?Sized), alliance: i64) -> Result<Option<Vec<Value>>, CallError> {
    Ok(dao::alliance(db, alliance).await.map_err(db_err)?.map(alliance_line))
}

async fn alliance_row(db: &Db, alliance: i64) -> CallResult {
    Ok(alliance_line_of(db, alliance).await?.map(|line| alliance_header().row(&line)).unwrap_or(Value::None))
}

const MEMBER_COLUMNS: &[&str] = &["corporationID", "allianceID", "chosenExecutorID", "startDate", "deleted"];

fn member_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("corporationID", DbType::I4),
        ("allianceID", DbType::I4),
        ("chosenExecutorID", DbType::I4),
        ("startDate", DbType::FileTime),
        ("deleted", DbType::Bool),
    ])
}

/// (corporationID, line) of each member corporation.
async fn member_lines(db: &(impl Executor + ?Sized), alliance: i64) -> Result<Vec<(i64, Vec<Value>)>, CallError> {
    let rows = dao::members(db, alliance).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let corp = r.corporation;
            let line = vec![
                Value::Int(corp),
                Value::Int(alliance),
                r.chosen_executor.map(Value::Int).unwrap_or(Value::None),
                Value::Int(r.joined),
                Value::Bool(false),
            ];
            (corp, line)
        })
        .collect())
}

const APPLICATION_COLUMNS: &[&str] = &["allianceID", "corporationID", "applicationText", "applicationDateTime", "state"];

pub(crate) fn application_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("allianceID", DbType::I4),
        ("corporationID", DbType::I4),
        ("applicationText", DbType::WStr),
        ("applicationDateTime", DbType::FileTime),
        ("state", DbType::I4),
    ])
}


pub(crate) fn application_line(r: dao::Application) -> Vec<Value> {
    vec![
        Value::Int(r.alliance),
        Value::Int(r.corporation),
        Value::unicode(r.text),
        Value::Int(r.created),
        Value::Int(r.state as i64),
    ]
}

pub(crate) async fn application_line_of(db: &(impl Executor + ?Sized), alliance: i64, corp: i64) -> Result<Option<Vec<Value>>, CallError> {
    Ok(dao::application(db, alliance, corp).await.map_err(db_err)?.map(application_line))
}

const RELATIONSHIP_COLUMNS: &[&str] = &["toID", "relationship"];

fn relationship_header() -> DbRowDescriptor {
    DbRowDescriptor::new([("toID", DbType::I4), ("relationship", DbType::I4)])
}

// ---------------------------------------------------------------- notifications

/// Every non-deleted character of the corporations in `alliance`, or of `corp` alone.
async fn pilots(db: &(impl Executor + ?Sized), alliance: Option<i64>, corp: Option<i64>) -> Result<Vec<i64>, CallError> {
    dao::pilots(db, alliance, corp).await.map_err(db_err)
}

/// Tell the alliance's online members (and `also_corp`'s) about a change.
async fn queue_tell<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, alliance: i64, also_corp: Option<i64>, event: &str, args: Vec<Value>) -> Result<(), CallError> {
    for char_id in pilots(db, Some(alliance), also_corp).await? {
        outbox.notify_pilot(char_id, event, args.clone());
    }
    Ok(())
}

async fn tell(db: &Db, world: &World, alliance: i64, also_corp: Option<i64>, event: &str, args: Vec<Value>) -> Result<(), CallError> {
    let mut outbox = Outbox::default();
    queue_tell(db, &mut outbox, alliance, also_corp, event, args).await?;
    dispatch_world((), outbox, world)
}

async fn queue_alliance_changed<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, alliance: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let new = alliance_line_of(db, alliance).await?;
    let ch = change(ALLIANCE_COLUMNS, old.as_deref(), new.as_deref());
    queue_tell(db, outbox, alliance, None, "OnAllianceChanged", vec![Value::Int(alliance), ch]).await
}

async fn alliance_changed(db: &Db, world: &World, alliance: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let mut outbox = Outbox::default();
    queue_alliance_changed(db, &mut outbox, alliance, old).await?;
    dispatch_world((), outbox, world)
}

pub(crate) async fn queue_application_changed<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, alliance: i64, corp: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let new = application_line_of(db, alliance, corp).await?;
    let ch = change(APPLICATION_COLUMNS, old.as_deref(), new.as_deref());
    queue_tell(db, outbox, alliance, Some(corp), "OnAllianceApplicationChanged", vec![Value::Int(alliance), Value::Int(corp), ch]).await
}


async fn queue_member_changed<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, alliance: i64, corp: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let new = member_lines(db, alliance).await?.into_iter().find(|(c, _)| *c == corp).map(|(_, l)| l);
    let ch = change(MEMBER_COLUMNS, old.as_deref(), new.as_deref());
    queue_tell(db, outbox, alliance, Some(corp), "OnAllianceMemberChanged", vec![Value::Int(alliance), Value::Int(corp), ch]).await
}

async fn member_changed(db: &Db, world: &World, alliance: i64, corp: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let mut outbox = Outbox::default();
    queue_member_changed(db, &mut outbox, alliance, corp, old).await?;
    dispatch_world((), outbox, world)
}

/// Put `corp` in `alliance` (or none): history, and every member's session.
pub(crate) async fn set_alliance<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, alliance: Option<i64>, now: i64) -> Result<(), CallError> {
    if dao::set_corporation_alliance(db, corp, alliance, now).await.map_err(db_err)? == 0 {
        return Err(notify_error(messages::TARGET_CORP_ALREADY_IN_ALLIANCE));
    }
    dao::insert_history(db, corp, alliance, now).await.map_err(db_err)?;
    let value = alliance.map(Value::Int).unwrap_or(Value::None);
    for char_id in pilots(db, None, Some(corp)).await? {
        outbox.notify_pilot(char_id, "__session", vec![Value::tuple([Value::str("allianceid"), value.clone()])]);
    }
    Ok(())
}

// ---------------------------------------------------------------- membership



/// Take `corp` out of its alliance: a new executor if it was one, or the alliance closes
/// with its last member.
pub(super) async fn leave_alliance<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, corp: i64, now: i64) -> Result<(), CallError> {
    let Some(r) = dao::corporation_membership(db, corp).await.map_err(db_err)?
    else {
        return Ok(());
    };
    let (alliance, executor) = (r.alliance, r.executor);
    let old_member = member_lines(db, alliance).await?.into_iter().find(|(c, _)| *c == corp).map(|(_, l)| l);
    let old_alliance = alliance_line_of(db, alliance).await?;
    let ch = change(MEMBER_COLUMNS, old_member.as_deref(), None);
    let recipients = pilots(db, Some(alliance), Some(corp)).await?;
    set_alliance(db, outbox, corp, None, now).await?;
    for char_id in recipients {
        outbox.notify_pilot(char_id, "OnAllianceMemberChanged", vec![Value::Int(alliance), Value::Int(corp), ch.clone()]);
    }
    dao::clear_support(db, alliance, corp).await.map_err(db_err)?;
    if executor == Some(corp) {
        // The largest remaining member takes over; none left closes the alliance.
        let next = dao::next_executor(db, alliance).await.map_err(db_err)?;
        match next {
            Some(next) => {
                dao::set_executor(db, alliance, next).await.map_err(db_err)?;
            }
            None => {
                dao::close_alliance(db, alliance).await.map_err(db_err)?;
                dao::delete_alliance_applications(db, alliance).await.map_err(db_err)?;
            }
        }
    }
    queue_alliance_changed(db, outbox, alliance, old_alliance).await?;
    Ok(())
}



/// The alliance a bound allianceRegistry is for (bindParams `(allianceID, isMaster)`).
fn bound_alliance(c: &Client, call: &Call<'_>) -> Option<i64> {
    call.bind_params
        .and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())))
        .or_else(|| c.session.int("allianceid"))
}



// ---------------------------------------------------------------- calls

/// Server suggestion algorithm shared by corporation tickers and alliance short names.
pub(crate) fn suggested_names(name: &str) -> Vec<String> {
    let initials: String = name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_ascii_alphanumeric())).collect::<String>().to_uppercase();
    let compact: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase();
    let mut out: Vec<String> = vec![];
    for t in [initials.chars().take(SUGGESTION_MAX_LENGTH).collect::<String>(), compact.chars().take(SUGGESTION_MAX_LENGTH).collect(), compact.chars().take(SUGGESTION_FALLBACK_LENGTHS[0]).collect(), compact.chars().take(SUGGESTION_FALLBACK_LENGTHS[1]).collect()] {
        if t.len() >= SUGGESTION_MIN_LENGTH && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}




pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "allianceRegistry" => alliance_registry::dispatch(c, call).await,
        _ => None,
    }
}
