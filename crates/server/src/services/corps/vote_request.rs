//! Typed `voteManager.InsertVoteCase` proposals for every supported vote kind.
//! The wire form is a five-tuple of option descriptors and numeric parameter slots.
//! It follows corporation/voting/voting.py and corp_ui_votes.py proposal forms and is parsed
//! once into an owned `Request` shared across transaction retries, so a retry never
//! deep-clones the option list. Wire `Value`s are never persisted: option text comes
//! from server messages and rule facts live in the numeric slots.
//! CEO elections also freeze the sitting CEO through the corporation's election
//! pointer; blueprint votes validate custody and reserve the item under item locks.

use super::*;
use crate::dao::corporations::{registry, shares as share_dao, votes as dao};
use infinity_core::consts::FILETIME_DAY;
use infinity_core::messages::corporations as text;
use crate::scheduler::{self, Job};
use crate::services::{has_role_or_is_ceo, market, value_field};
use std::sync::Arc;

/// corporation/voting/voting.py vote types; this is the canonical registry.
/// War votes stay unsupported.
pub(super) const VOTE_CEO: i64 = 0;
pub(super) const VOTE_WAR: i64 = 1;
pub(super) const VOTE_SHARES: i64 = 2;
pub(super) const VOTE_KICK: i64 = 3;
pub(super) const VOTE_GENERAL: i64 = 4;
pub(super) const VOTE_LOCKDOWN: i64 = 5;
pub(super) const VOTE_UNLOCK: i64 = 6;

/// Non-general options: the affirmative option ID and the dissenting one.
pub(super) const OPTION_POSITIVE: i32 = 0;
pub(super) const OPTION_NEGATIVE: i32 = 1;

/// corporation/voting/voting.py case limits.
const MAX_DAYS: i64 = 30;
/// corp_ui_votes.py: CEO elections accept one to five days.
const MAX_CEO_DAYS: i64 = 5;
/// corp_ui_votes.py: general vote form limits.
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 50;
const MAX_TITLE: usize = 100;
const MAX_DESCRIPTION: usize = 1000;
const MAX_OPTION: usize = 100;
/// corp_ui_votes.py: the CEO candidacy form allows a longer motion.
const MAX_CEO_TITLE: usize = 500;
const MAX_CEO_DESCRIPTION: usize = 100;
/// corp_ui_votes.py:519-525 share issuance range.
const MAX_SHARE_ISSUE: i64 = 10_000_000;
/// Server policy: a CEO candidate must personally hold five percent of issued shares.
const CEO_SHARE_PERCENT: i128 = 5;
/// corporation/voting/voting.py descriptor labels live under this UI key prefix.
const LABEL_PREFIX: &str = "UI/Corporations/CorporationWindow/Politics/";

fn invalid() -> CallError { votes::invalid() }
fn unsupported() -> CallError { notify_error(text::VOTE_UNSUPPORTED) }

/// One parsed option row as it will be persisted; texts are owned so the request
/// survives transaction retries without touching the wire values again.
pub(super) struct ProposalOption {
    pub option_id: i32,
    pub text: String,
    pub parameter: Option<i64>,
    pub parameter1: Option<i64>,
    pub parameter2: Option<i64>,
}

/// The kind-specific fact of a non-general proposal.
pub(super) enum Kind {
    General,
    Ceo { candidate: i64 },
    Shares,
    Kick { target: i64 },
    Lock { target: item_locks::Target },
    Unlock { target: item_locks::Target },
}

/// A fully parsed `InsertVoteCase` call, shared across transaction retries.
pub(super) struct Request {
    pub title: String,
    pub description: String,
    pub vote_type: i64,
    pub days: i64,
    pub options: Vec<ProposalOption>,
    pub kind: Kind,
}

fn text_arg<'a>(call: &'a Call<'_>, index: usize) -> Result<&'a str, CallError> {
    match call.args.get(index) { Some(Value::Unicode(value)) => Ok(value), _ => Err(invalid()) }
}
fn int_arg(call: &Call<'_>, index: usize) -> Result<i64, CallError> {
    match call.args.get(index) { Some(Value::Int(value)) => Ok(*value), _ => Err(invalid()) }
}
fn none_arg(call_arg: Option<&Value>) -> Result<(), CallError> {
    match call_arg { Some(Value::None) => Ok(()), _ => Err(invalid()) }
}

/// A `(label[, kwargs])` descriptor: validate the label and return the optional kwargs.
/// Labels and kwargs describe the client's own option rendering; the numeric slots
/// carry the rule facts, so unknown or absent extra kwargs are ignored.
fn descriptor<'a>(value: &'a Value, label: &str) -> Result<Option<&'a Value>, CallError> {
    let items = value.items().ok_or_else(invalid)?;
    if !(1..=2).contains(&items.len()) { return Err(invalid()); }
    let Some(text_value) = items[0].as_text() else { return Err(invalid()) };
    if text_value.strip_prefix(LABEL_PREFIX) != Some(label) { return Err(invalid()); }
    Ok(items.get(1))
}

/// When the descriptor carries a kwargs dict, its named number must equal the wire
/// parameter the rules will use; absent kwargs never override it.
fn descriptor_number(kwargs: Option<&Value>, key: &str, expected: i64) -> Result<(), CallError> {
    let Some(kwargs) = kwargs else { return Ok(()) };
    match value_field(kwargs, key) {
        Some(value) if value.as_int() == Some(expected) => Ok(()),
        _ => Err(invalid()),
    }
}

fn proposal_option(option_id: i32, text: &'static str, parameter: Option<i64>, parameter1: Option<i64>, parameter2: Option<i64>) -> ProposalOption {
    ProposalOption { option_id, text: text.into(), parameter, parameter1, parameter2 }
}

/// Parse one `InsertVoteCase` call for `actor`. Strict about argument count, types
/// and `None` slots: out-of-range IDs are refused rather than truncated.
pub(super) fn parse(call: &Call<'_>, actor: i64) -> Result<Request, CallError> {
    if call.args.len() != 5 { return Err(invalid()); }
    let title = text_arg(call, 0)?;
    let description = text_arg(call, 1)?;
    let vote_type = int_arg(call, 2)?;
    let options = &call.args[3];
    let days = int_arg(call, 4)?;
    let (options, kind) = match vote_type {
        VOTE_GENERAL => {
            let items = options.items().ok_or_else(invalid)?;
            let options = items.iter().enumerate().map(|(index, value)| match value {
                Value::Unicode(text) => Ok(ProposalOption {
                    option_id: i32::try_from(index).map_err(|_| invalid())?, text: text.clone(),
                    parameter: None, parameter1: None, parameter2: None,
                }),
                _ => Err(invalid()),
            }).collect::<Result<Vec<_>, CallError>>()?;
            (options, Kind::General)
        }
        VOTE_WAR => return Err(unsupported()),
        VOTE_CEO | VOTE_SHARES | VOTE_KICK | VOTE_LOCKDOWN | VOTE_UNLOCK => {
            let (yes_label, no_label, yes_text, no_text) = match vote_type {
                VOTE_CEO => ("SomeoneForCEO", "DontChangeCEO", text::VOTE_OPTION_YES_CEO, text::VOTE_OPTION_NO_CEO),
                VOTE_SHARES => ("CreateShares", "DoNotCreateShares", text::VOTE_OPTION_YES_SHARES, text::VOTE_OPTION_NO_SHARES),
                VOTE_KICK => ("ExpelFromCorporation", "DontExpelFromCorporation", text::VOTE_OPTION_YES_KICK, text::VOTE_OPTION_NO_KICK),
                VOTE_LOCKDOWN => ("LockdownItem", "DontLockdownItem", text::VOTE_OPTION_YES_LOCKDOWN, text::VOTE_OPTION_NO_LOCKDOWN),
                _ => ("UnlockItem", "DontUnlockItem", text::VOTE_OPTION_YES_UNLOCK, text::VOTE_OPTION_NO_UNLOCK),
            };
            let items = options.items().ok_or_else(invalid)?;
            if items.len() != 5 { return Err(invalid()); }
            descriptor(&items[1], no_label)?;
            let yes_kwargs = descriptor(&items[0], yes_label)?;
            let (parameter, parameter1, parameter2, kind) = match vote_type {
                VOTE_CEO => {
                    let candidate = items[2].as_int().ok_or_else(invalid)?;
                    none_arg(items.get(3))?;
                    none_arg(items.get(4))?;
                    // voting.py: the candidacy form always nominates the caller.
                    if candidate != actor { return Err(invalid()); }
                    descriptor_number(yes_kwargs, "char", candidate)?;
                    (Some(candidate), None, None, Kind::Ceo { candidate })
                }
                VOTE_SHARES => {
                    let quantity = items[2].as_int().ok_or_else(invalid)?;
                    none_arg(items.get(3))?;
                    none_arg(items.get(4))?;
                    if !(1..=MAX_SHARE_ISSUE).contains(&quantity) { return Err(invalid()); }
                    descriptor_number(yes_kwargs, "shares", quantity)?;
                    (Some(quantity), None, None, Kind::Shares)
                }
                VOTE_KICK => {
                    let target = items[2].as_int().ok_or_else(invalid)?;
                    none_arg(items.get(3))?;
                    none_arg(items.get(4))?;
                    if target <= 0 { return Err(invalid()); }
                    descriptor_number(yes_kwargs, "char", target)?;
                    (Some(target), None, None, Kind::Kick { target })
                }
                _ => {
                    let item_id = items[2].as_int().ok_or_else(invalid)?;
                    let type_id = i32::try_from(items[3].as_int().ok_or_else(invalid)?).map_err(|_| invalid())?;
                    let station_id = items[4].as_int().ok_or_else(invalid)?;
                    if item_id <= 0 || type_id <= 0 || station_id <= 0 { return Err(invalid()); }
                    descriptor_number(yes_kwargs, "item", i64::from(type_id))?;
                    let target = item_locks::Target { item_id, type_id, station_id };
                    let kind = if vote_type == VOTE_LOCKDOWN { Kind::Lock { target } } else { Kind::Unlock { target } };
                    (Some(item_id), Some(i64::from(type_id)), Some(station_id), kind)
                }
            };
            let options = vec![
                proposal_option(OPTION_POSITIVE, yes_text, parameter, parameter1, parameter2),
                proposal_option(OPTION_NEGATIVE, no_text, Some(0), parameter1, parameter2),
            ];
            (options, kind)
        }
        _ => return Err(unsupported()),
    };
    Ok(Request { title: title.into(), description: description.into(), vote_type, days, options, kind })
}

fn valid_text(value: &str, max: usize, required: bool) -> bool {
    value.chars().count() <= max && (!required || !value.trim().is_empty())
}

/// Validate the free-text and duration limits the client's form enforces.
fn valid_request(request: &Request) -> bool {
    match request.vote_type {
        VOTE_GENERAL => {
            valid_text(&request.title, MAX_TITLE, true) && valid_text(&request.description, MAX_DESCRIPTION, false)
                && (MIN_OPTIONS..=MAX_OPTIONS).contains(&request.options.len())
                && request.options.iter().all(|option| valid_text(&option.text, MAX_OPTION, true))
                && (1..=MAX_DAYS).contains(&request.days)
        }
        VOTE_CEO => {
            valid_text(&request.title, MAX_CEO_TITLE, true) && valid_text(&request.description, MAX_CEO_DESCRIPTION, false)
                && (1..=MAX_CEO_DAYS).contains(&request.days)
        }
        _ => {
            valid_text(&request.title, MAX_TITLE, true) && valid_text(&request.description, MAX_DESCRIPTION, false)
                && (1..=MAX_DAYS).contains(&request.days)
        }
    }
}

/// Whether `candidate` may stand for CEO of `issuer` at `now`: a live member who is
/// not the sitting CEO, without an active election, at the configured effective
/// Corporation Management level and holding at least five percent of issued shares
/// personally. A missing issued total is an internal fault, never an assumed number.
pub(super) async fn can_run_for_ceo(ex: &(impl Executor + ?Sized), sde: &Sde, candidate: i64, issuer: i64, now: i64) -> Result<bool, CallError> {
    if !is_player_corp(issuer) { return Ok(false); }
    if dao::election(ex, issuer).await.map_err(db_err)?.is_some() { return Ok(false); }
    if !ceo::eligible(ex, sde, issuer, candidate, now).await? { return Ok(false); }
    let personal = share_dao::balance(ex, issuer, candidate).await.map_err(db_err)?;
    let Some(total) = share_dao::issued_total(ex, issuer).await.map_err(db_err)? else {
        return Err(CallError::Internal(anyhow::anyhow!("corporation share issuance has not been initialized")));
    };
    Ok(i128::from(personal) * 100 >= i128::from(total) * CEO_SHARE_PERCENT)
}

/// Create the parsed request inside one market-locked, retried transaction:
/// admission locks before any case or item lock, business `now` sampled after the
/// locks, then the case, options, snapshot, election pointer or item reservation,
/// and the durable close job.
async fn create_inner<X: Send>(ex: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, actor: i64, request: &Request) -> Result<i64, CallError> {
    if !valid_request(request) { return Err(invalid()); }
    let (issuer, _) = membership(ex, actor).await?;
    let director_required = request.vote_type != VOTE_CEO;
    if !is_player_corp(issuer) || (director_required && !has_role_or_is_ceo(ex, actor, issuer, ROLE_DIRECTOR).await?) {
        return Err(notify_error(text::VOTE_AUTHORITY_REQUIRED));
    }
    // Collect every organization and character the creation may touch before the
    // first lock: the corporation, the actor, the sitting CEO an election freezes,
    // and the member an expulsion proposal names.
    let mut characters = vec![actor];
    let previous_ceo = if request.vote_type == VOTE_CEO {
        registry::ceo_of(ex, issuer).await.map_err(db_err)?
    } else {
        None
    };
    if let Some(previous) = previous_ceo { characters.push(previous); }
    if let Kind::Kick { target } = &request.kind { characters.push(*target); }
    if !registry::lock_admission(ex, &[], &[issuer], &characters).await.map_err(db_err)? {
        return Err(CallError::Internal(anyhow::anyhow!("corporation association changed during vote creation")));
    }
    let (fresh_issuer, _) = membership(ex, actor).await?;
    if fresh_issuer != issuer || (director_required && !has_role_or_is_ceo(ex, actor, issuer, ROLE_DIRECTOR).await?) {
        return Err(notify_error(text::VOTE_AUTHORITY_REQUIRED));
    }
    let corporation = registry::corporation(ex, issuer).await.map_err(db_err)?
        .filter(|row| !row.deleted)
        .ok_or_else(|| notify_error(text::VOTE_AUTHORITY_REQUIRED))?;
    if request.vote_type == VOTE_CEO && corporation.ceo_id != previous_ceo {
        // The pre-lock identity fed the character lock set; a moved CEO means the
        // freeze would record the wrong previous holder.
        return Err(CallError::Internal(anyhow::anyhow!("corporation ceo changed during vote creation")));
    }
    let now = filetime_now();
    let ends = request.days.checked_mul(FILETIME_DAY).and_then(|duration| now.checked_add(duration)).ok_or_else(invalid)?;
    let mut new_case = dao::NewCase {
        corporation_id: issuer, creator_id: actor, title: &request.title, description: &request.description,
        vote_type: request.vote_type, starts: now, ends, previous_ceo_id: None, previous_ceo_was_active: None,
    };
    let mut frozen_ceo = None;
    match &request.kind {
        Kind::Ceo { candidate } => {
            let Some(previous) = previous_ceo else { return Err(notify_error(text::VOTE_NOT_ELIGIBLE)) };
            if !can_run_for_ceo(ex, sde, *candidate, issuer, now).await? {
                return Err(notify_error(text::VOTE_NOT_ELIGIBLE));
            }
            let was_active = permissions::is_active_ceo(ex, previous, issuer).await?;
            new_case.previous_ceo_id = Some(previous);
            new_case.previous_ceo_was_active = Some(was_active);
            frozen_ceo = Some((previous, was_active));
        }
        Kind::Lock { target } => {
            let _locks = item_locks::admit_target(ex, sde, issuer, *target, item_locks::Action::Lock, None, now).await?;
        }
        Kind::Unlock { target } => {
            let _locks = item_locks::admit_target(ex, sde, issuer, *target, item_locks::Action::Unlock, None, now).await?;
        }
        Kind::Kick { target } => {
            // The proposal names a real member of this corporation who is not its
            // CEO; roles, presence and the exit cooldown stay activation policy.
            let Some(authority) = permissions::load(ex, *target).await? else {
                return Err(notify_error(text::VOTE_TARGET_INVALID));
            };
            if !authority.live || authority.corporation != issuer || authority.ceo == Some(*target) {
                return Err(notify_error(text::VOTE_TARGET_INVALID));
            }
        }
        _ => {}
    }
    if dao::open_count(ex, issuer, now).await.map_err(db_err)? >= votes::MAX_OPEN { return Err(invalid()); }
    let id = dao::create(ex, &new_case).await.map_err(db_err)?;
    for option in &request.options {
        dao::add_option(ex, id, &dao::NewOption {
            option_id: option.option_id, text: &option.text,
            parameter: option.parameter, parameter1: option.parameter1, parameter2: option.parameter2,
        }).await.map_err(db_err)?;
    }
    dao::snapshot(ex, id, issuer).await.map_err(db_err)?;
    match &request.kind {
        Kind::Ceo { .. } => {
            let (previous, was_active) = frozen_ceo.expect("ceo request records the sitting ceo");
            if !dao::begin_election(ex, issuer, id, previous).await.map_err(db_err)? {
                return Err(notify_error(text::VOTE_ELECTION_ACTIVE));
            }
            // Session role values and the persisted revocation notice go only to a
            // CEO who truly lost active authority; an already-inactive CEO's
            // projection does not change, so nothing is queued for them.
            if was_active {
                ceo::role_projection_changed_in(ex, outbox, issuer, previous).await?;
                notifications::ceo_revoked(ex, outbox, previous, issuer).await?;
            }
        }
        Kind::Lock { target } | Kind::Unlock { target } => {
            if !dao::reserve_item(ex, id, target.item_id).await.map_err(db_err)? {
                return Err(CallError::Internal(anyhow::anyhow!("blueprint reservation failed for a new vote case")));
            }
        }
        _ => {}
    }
    let job = Job { id: format!("{}:{id}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: ends, payload: id.to_string() };
    scheduler::schedule(ex, &job).await.map_err(db_err)?;
    votes::notify(ex, outbox, issuer, id, "OnVoteCaseChanged").await?;
    Ok(id)
}

/// The `InsertVoteCase` entry point: market lock, one retried transaction over the
/// shared parsed request, post-commit dispatch and scheduler wake.
pub(super) async fn create(db: &Db, world: &World, sde: &Arc<Sde>, actor: i64, request: Arc<Request>) -> Result<i64, CallError> {
    let _guard = market::lock().await;
    db.transaction(|tx, outbox: &mut Outbox| {
        let request = Arc::clone(&request);
        let sde = Arc::clone(sde);
        Box::pin(async move {
            create_inner(tx, &sde, outbox, actor, &request).await
        })
    }, |value, outbox| async move {
        let result = dispatch_world(value, outbox, world);
        scheduler::wake();
        result
    }).await
}
