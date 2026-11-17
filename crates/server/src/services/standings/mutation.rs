//! Shared standing mutation writer: the one path that changes `game.standings`.
//!
//! Every cause — GM slash command, server script, mission outcome or bulk raise —
//! supplies `Change`s and a `Mutation` describing when and why. `apply_tx` runs
//! inside the caller's `db::transaction` closure against the real transaction and
//! queues `OnStandingSet` notifications in its outbox, so standing value, history
//! row and notification commit or roll back with the caller's other writes.
//! Completed missions scale only positive configured gain rates by the pilot's
//! effective Social contribution and character socialBonus multiplier. Booster
//! dogma effects raise socialBonus; decline and quit penalties are unchanged.
//!
//! The writer validates all inputs before any write, verifies the character is
//! not deleted before and after `selection::lock_character`, then reads, updates
//! and journals each owner in ascending ID order (lock order for `game.standings`
//! rows). An owner whose computed value equals the old value gets no write,
//! history row or notification. `event_type_id` values are the client's
//! `eve/common/script/mgt/appLogConst.py` constants; mission causes store the
//! mission's localization `name_id`, never its template ID.

use super::{db_err, notify_error, CallError, STANDING_LIMIT};
use crate::dao::characters::{lookup, selection, standings};
use crate::db::outbox::Outbox;
use crate::services::fit::{self, Fit, Key};
use infinity_core::consts::{SKILL_SOCIAL, dogma::{ATTR_SKILL_LEVEL, ATTR_SOCIAL_BONUS, ATTR_SOCIAL_MUTATOR}};
use infinity_core::messages;
use marshal::Value;
use crate::db::Transaction;

/// Client `appLogConst.eventStandingSlashSet`: standing set by a GM slash command.
pub(super) const EVENT_SLASH_SET: i64 = 84;
/// Client `appLogConst.eventStandingAgentMissionCompleted`: mission finished for the agent.
pub(super) const EVENT_MISSION_COMPLETED: i64 = 73;
/// Client `appLogConst.eventStandingAgentMissionDeclined`: offered mission declined.
pub(super) const EVENT_MISSION_DECLINED: i64 = 75;
/// Client `appLogConst.eventStandingAgentMissionFailed`: accepted mission quit or failed.
pub(super) const EVENT_MISSION_FAILED: i64 = 74;
/// Client `appLogConst.eventStandingUpdate`: standing changed by a server script.
pub(super) const EVENT_SCRIPT_UPDATE: i64 = 45;

/// How one owner's standing is computed.
#[derive(Clone, Copy)]
pub(crate) enum Operation {
    /// Absolute value, validated finite and inside -10..=10 before any write.
    Set(f64),
    /// Fraction of the remaining way to +10 (positive) or -10 (negative), the
    /// client's standingsvc `modification` formula, clamped to the range.
    TowardLimit(f64),
}

/// One owner standing to change; callers pass a slice sorted by unique `from_id`.
#[derive(Clone, Copy)]
pub(crate) struct Change {
    pub from_id: i64,
    pub operation: Operation,
}

/// Why the batch is applied; maps to the history `event_type_id` and the
/// `actor_id` / `mission_name_id` / `int_1` wire field, plus the fallback `msg`.
#[derive(Clone, Copy)]
pub(crate) enum Cause<'a> {
    /// A GM slash command; `actor` is the GM's character, `reason` its free text.
    Slash { actor: i64, reason: &'a str },
    /// A server-side script (Lua host function); `actor` is the calling character if known.
    Script { actor: Option<i64> },
    MissionCompleted { name_id: Option<i64> },
    MissionDeclined { name_id: Option<i64> },
    MissionFailed { name_id: Option<i64> },
}

/// One atomic batch of standing changes.
#[derive(Clone, Copy)]
pub(crate) struct Mutation<'a> {
    /// Client FILETIME captured once before the transaction; journal and wire
    /// agree even when the lock waits.
    pub occurred_at: i64,
    pub cause: Cause<'a>,
}

impl<'a> Cause<'a> {
    fn event_type_id(self) -> i64 {
        match self {
            Cause::Slash { .. } => EVENT_SLASH_SET,
            Cause::Script { .. } => EVENT_SCRIPT_UPDATE,
            Cause::MissionCompleted { .. } => EVENT_MISSION_COMPLETED,
            Cause::MissionDeclined { .. } => EVENT_MISSION_DECLINED,
            Cause::MissionFailed { .. } => EVENT_MISSION_FAILED,
        }
    }
    fn actor_id(self) -> Option<i64> {
        match self {
            Cause::Slash { actor, .. } => Some(actor),
            Cause::Script { actor } => actor,
            Cause::MissionCompleted { .. } | Cause::MissionDeclined { .. } | Cause::MissionFailed { .. } => None,
        }
    }
    fn mission_name_id(self) -> Option<i64> {
        match self {
            Cause::MissionCompleted { name_id } | Cause::MissionDeclined { name_id } | Cause::MissionFailed { name_id } => name_id,
            Cause::Slash { .. } | Cause::Script { .. } => None,
        }
    }
    /// `msg` the client shows for this event: the slash reason when given,
    /// otherwise the shared fallback for the cause.
    fn message(self) -> &'a str {
        match self {
            Cause::Slash { reason, .. } if !reason.is_empty() => reason,
            Cause::Slash { .. } => messages::standings::REASON_GM_COMMAND,
            Cause::Script { .. } => messages::standings::REASON_SCRIPT,
            Cause::MissionCompleted { .. } | Cause::MissionDeclined { .. } | Cause::MissionFailed { .. } => messages::standings::REASON_MISSION,
        }
    }
}

/// Apply the batch to `character` inside the caller's transaction; returns the
/// number of owners whose standing actually changed. `changes` must be sorted
/// ascending by `from_id` with no duplicates; zero owners and zero rates are
/// skipped.
pub(crate) async fn apply_tx<X: Send>(
    tx: &Transaction<'_>,
    outbox: &mut Outbox<X>,
    character: i64,
    changes: &[Change],
    mutation: Mutation<'_>,
) -> Result<i64, CallError> {
    // Validate every input before touching the character row or any owner.
    for change in changes {
        if !valid(change.operation) {
            return Err(notify_error(messages::standings::STANDING_OUT_OF_RANGE));
        }
    }
    if lookup::find(tx, "", Some(character)).await.map_err(db_err)?.is_none() {
        return Err(notify_error(messages::standings::HISTORY_INVALID));
    }
    selection::lock_character(tx, character).await.map_err(db_err)?;
    // A deletion committed while the lock waited leaves the row flagged deleted.
    if lookup::find(tx, "", Some(character)).await.map_err(db_err)?.is_none() {
        return Err(notify_error(messages::standings::HISTORY_INVALID));
    }
    let mut changed = 0_i64;
    for change in changes {
        if change.from_id == 0 {
            continue;
        }
        let rate = match change.operation {
            Operation::Set(_) => None,
            Operation::TowardLimit(rate) => Some(rate),
        };
        if rate == Some(0.0) {
            continue;
        }
        let old = standings::standing(tx, character, change.from_id).await.map_err(db_err)?.unwrap_or(0.0);
        let new = match change.operation {
            Operation::Set(value) => value,
            Operation::TowardLimit(rate) => {
                let moved = if rate > 0.0 { old + (STANDING_LIMIT - old) * rate } else { old + (STANDING_LIMIT + old) * rate };
                moved.clamp(-STANDING_LIMIT, STANDING_LIMIT)
            }
        };
        if new == old {
            continue;
        }
        standings::set(tx, character, change.from_id, new).await.map_err(db_err)?;
        standings::history::insert(tx, &standings::history::NewEntry {
            char_id: character,
            from_id: change.from_id,
            old_standing: old,
            new_standing: new,
            event_type_id: mutation.cause.event_type_id(),
            occurred_at: mutation.occurred_at,
            actor_id: mutation.cause.actor_id(),
            mission_name_id: mutation.cause.mission_name_id(),
            message: mutation.cause.message(),
        }).await.map_err(db_err)?;
        outbox.notify_pilot(character, "OnStandingSet", vec![Value::Int(change.from_id), Value::Int(character), Value::Float(new)]);
        changed += 1;
    }
    Ok(changed)
}

/// The pilot's positive standing-gain multiplier for completed missions:
/// `1 + socialMutator/100` of an effective positive-level Social skill, times
/// the character's effective `socialBonus`. Social's effect scales
/// `socialMutator` by its level, so the effective attribute already carries the
/// per-level growth; a zero-level or absent skill contributes no gain.
pub(crate) fn mission_gain_multiplier(fit: &Fit) -> f64 {
    let social = if fit.value(Key::Skill(SKILL_SOCIAL), ATTR_SKILL_LEVEL) <= 0.0 {
        0.0
    } else {
        fit.value(Key::Skill(SKILL_SOCIAL), ATTR_SOCIAL_MUTATOR)
    };
    (1.0 + social / fit::PERCENT_SCALE) * fit.value(Key::Char, ATTR_SOCIAL_BONUS)
}

/// Journal the standing consequences of a completed mission for `agent` of
/// `level`, using the operator-configured gain rates scaled by
/// `gain_multiplier`.
pub(crate) async fn mission_completed<X: Send>(
    tx: &Transaction<'_>,
    outbox: &mut Outbox<X>,
    character: i64,
    agent: i64,
    corporation: i64,
    faction: i64,
    level: i64,
    gain_multiplier: f64,
    name_id: Option<i64>,
    occurred_at: i64,
) -> Result<(), CallError> {
    let policy = &crate::config::game().standings;
    let gain = policy.mission_corp_gain * level.max(1) as f64;
    let changes = sorted([
        (agent, mission_rate(gain * policy.mission_agent_gain_multiplier, gain_multiplier)),
        (corporation, mission_rate(gain, gain_multiplier)),
        (faction, mission_rate(gain / policy.mission_faction_gain_divisor, gain_multiplier)),
    ]);
    apply_tx(tx, outbox, character, &changes, Mutation { occurred_at, cause: Cause::MissionCompleted { name_id } }).await.map(|_| ())
}

/// Journal the standing consequences of declining (`quit == false`) or quitting
/// (`quit == true`) a mission for `agent`.
pub(crate) async fn mission_declined<X: Send>(
    tx: &Transaction<'_>,
    outbox: &mut Outbox<X>,
    character: i64,
    agent: i64,
    corporation: i64,
    name_id: Option<i64>,
    quit: bool,
    occurred_at: i64,
) -> Result<(), CallError> {
    let loss = crate::config::game().standings.decline_loss;
    let changes = sorted([(agent, loss), (corporation, loss)]);
    let cause = if quit { Cause::MissionFailed { name_id } } else { Cause::MissionDeclined { name_id } };
    apply_tx(tx, outbox, character, &changes, Mutation { occurred_at, cause }).await.map(|_| ())
}

/// Pre-write check shared by `apply_tx`: `Set` values must be finite inside the
/// client range; `TowardLimit` rates must be finite.
fn valid(operation: Operation) -> bool {
    match operation {
        Operation::Set(value) => value.is_finite() && (-STANDING_LIMIT..=STANDING_LIMIT).contains(&value),
        Operation::TowardLimit(rate) => rate.is_finite(),
    }
}

/// Sort the wrappers' owner rates ascending for `apply_tx`, in place on the
/// stack. The agent/corporation/faction ID domains do not overlap, so callers
/// never supply duplicate `from_id`s and no dedup is needed; zero owners and
/// zero rates are left in place for `apply_tx` to skip.
fn sorted<const N: usize>(mut changes: [(i64, f64); N]) -> [Change; N] {
    changes.sort_unstable_by_key(|(owner, _)| *owner);
    changes.map(|(from_id, rate)| Change { from_id, operation: Operation::TowardLimit(rate) })
}

/// `gain_multiplier` scales only positive gain rates; zero and negative rates
/// are penalties and pass through unchanged. A non-finite multiplier propagates
/// into the rate, where `apply_tx` validation refuses it before any write.
fn mission_rate(rate: f64, gain_multiplier: f64) -> f64 {
    if rate > 0.0 { rate * gain_multiplier } else { rate }
}

#[cfg(all(test, feature = "tests-social"))]
mod tests;
