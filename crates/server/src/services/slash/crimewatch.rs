//! GM `/cwatch` and `/naughty` commands, forwarded by the slash service:
//! inspect or reset the caller's own Crimewatch combat timers, save the
//! caller's persistent safety preference, and request the ordinary CONCORD
//! response in high security space. Parse and reply selection only: the
//! shared Crimewatch control service owns every state change, the slash role
//! gate owns authorization, and the caller's selected character is always the
//! target. Timer deadlines are in-memory FILETIME values; the safety
//! preference is persisted in `game.characters`.
use super::{args, slash_error, CallError, Client, QUANTITY_RADIX};
use infinity_core::{consts::{FILETIME_HOUR, FILETIME_MINUTE, FILETIME_SECOND}, messages, session::OnlineSession};
use crate::client::ms_from_filetime_ticks;
use crate::services::crimewatch::control::{self, NaughtyOutcome, SafetyLevel, Snapshot, TimerChange, TimerKind};
use crate::services::identity::Identity;

/// `/cwatch <kind>` names one of the six combat timers.
fn timer_kind(name: &str) -> Option<TimerKind> {
    if name.eq_ignore_ascii_case("weapon") { Some(TimerKind::Weapons) }
    else if name.eq_ignore_ascii_case("pvp") { Some(TimerKind::Pvp) }
    else if name.eq_ignore_ascii_case("npc") { Some(TimerKind::Npc) }
    else if name.eq_ignore_ascii_case("criminal") { Some(TimerKind::Criminal) }
    else if name.eq_ignore_ascii_case("suspect") { Some(TimerKind::Suspect) }
    else if name.eq_ignore_ascii_case("disapproval") { Some(TimerKind::Disapproval) }
    else { None }
}

/// `/cwatch safety <level>` names the persisted preference.
fn safety_level(name: &str) -> Option<SafetyLevel> {
    if name.eq_ignore_ascii_case("full") { Some(SafetyLevel::Full) }
    else if name.eq_ignore_ascii_case("partial") { Some(SafetyLevel::Partial) }
    else if name.eq_ignore_ascii_case("none") { Some(SafetyLevel::None) }
    else { None }
}

/// `on` refreshes a timer with its configured default duration.
fn default_ms(kind: TimerKind) -> i64 {
    let config = crate::config::game();
    let seconds = match kind {
        TimerKind::Weapons => config.crimewatch.weapons_timer_secs,
        TimerKind::Pvp => config.crimewatch.pvp_timer_secs,
        TimerKind::Npc => config.crimewatch.npc_timer_secs,
        TimerKind::Criminal => config.crimewatch.criminal_timer_secs,
        TimerKind::Suspect => config.bounty.kill_right_suspect_secs,
        TimerKind::Disapproval => config.crimewatch.disapproval_timer_secs,
    };
    ms_from_filetime_ticks(seconds.saturating_mul(FILETIME_SECOND))
}

/// Exact decimal duration in milliseconds: ASCII digits with an optional
/// single `.` between a nonempty integer and fraction part, and an optional
/// `ms`, `s`, `m` or `h` suffix (seconds without one). The result must be a
/// whole number of milliseconds; sign characters, scientific notation,
/// sub-millisecond fractions and values that overflow are refused.
fn duration_ms(text: &str) -> Option<i64> {
    let (digits, multiplier) = if let Some(rest) = text.strip_suffix("ms") {
        (rest, 1)
    } else if let Some(rest) = text.strip_suffix('s') {
        (rest, ms_from_filetime_ticks(FILETIME_SECOND))
    } else if let Some(rest) = text.strip_suffix('m') {
        (rest, ms_from_filetime_ticks(FILETIME_MINUTE))
    } else if let Some(rest) = text.strip_suffix('h') {
        (rest, ms_from_filetime_ticks(FILETIME_HOUR))
    } else {
        (text, ms_from_filetime_ticks(FILETIME_SECOND))
    };
    let (integer, fraction) = match digits.split_once('.') {
        Some((integer, fraction)) if !integer.is_empty() && !fraction.is_empty() => (integer, fraction),
        Some(_) => return None,
        None => (digits, ""),
    };
    if !integer.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Trailing zeros keep value/10^len exact; the remainder must divide into
    // whole milliseconds.
    let fraction = fraction.trim_end_matches('0');
    let fraction_ms = if fraction.is_empty() { 0 } else {
        let denominator = (0..fraction.len()).try_fold(1i128, |d, _| d.checked_mul(i128::from(QUANTITY_RADIX)))?;
        let numerator = i128::from(fraction.parse::<i64>().ok()?) * i128::from(multiplier);
        if numerator % denominator != 0 { return None; }
        i64::try_from(numerator / denominator).ok()?
    };
    integer.parse::<i64>().ok()?.checked_mul(multiplier)?.checked_add(fraction_ms)
}

/// The six timer deadlines in the status builder's fixed order.
fn status_text(character: i64, snapshot: &Snapshot) -> String {
    messages::gm::gm_cwatch_status(character, [
        snapshot.weapons, snapshot.pvp, snapshot.npc,
        snapshot.criminal, snapshot.suspect, snapshot.disapproval,
    ], snapshot.safety as i64)
}

/// `/cwatch` and `/cwatch status` report the caller's state.
async fn status(c: &mut Client) -> Result<String, CallError> {
    let me = args::selected_character(&c.session)?;
    Ok(status_text(me, &control::snapshot(&c.db, me).await?))
}

/// `/naughty` and `/cwatch naughty` share this operation; the service verifies
/// the caller's live ship and session inside the actor.
async fn naughty(c: &mut Client) -> Result<String, CallError> {
    let Some(expected) = OnlineSession::from_session(&c.session) else {
        return Err(slash_error(messages::gm::GM_ARGS_NO_CHARACTER.into()));
    };
    let identity = Identity::of_client(c);
    Ok(match control::naughty(&c.db, &c.world, &identity, &expected).await? {
        NaughtyOutcome::Queued => messages::gm::GM_CWATCH_NAUGHTY_QUEUED,
        NaughtyOutcome::Ignored => messages::gm::GM_CWATCH_NAUGHTY_IGNORED,
        NaughtyOutcome::NotHighsec => messages::gm::GM_CWATCH_NAUGHTY_NOT_HIGHSEC,
    }.into())
}

/// `/cwatch` routes its optional subcommand; `/naughty` takes no arguments.
pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    let usage = || slash_error(messages::gm::GM_CWATCH_USAGE.into());
    if command == "naughty" {
        return if args.is_empty() { naughty(c).await } else { Err(slash_error(messages::gm::GM_NAUGHTY_USAGE.into())) };
    }
    match args {
        [] => status(c).await,
        [arg] if arg.eq_ignore_ascii_case("status") => status(c).await,
        [arg] if arg.eq_ignore_ascii_case("clear") => {
            let me = args::selected_character(&c.session)?;
            control::clear(&c.db, &c.world, me).await?;
            Ok(messages::gm::GM_CWATCH_CLEARED.into())
        }
        [arg] if arg.eq_ignore_ascii_case("naughty") => naughty(c).await,
        [arg, level] if arg.eq_ignore_ascii_case("safety") => {
            let Some(level) = safety_level(level) else { return Err(usage()) };
            let saved = control::set_safety(&c.db, args::selected_character(&c.session)?, level).await?;
            Ok(messages::gm::gm_cwatch_safety_saved(saved as i64))
        }
        [name, setting] => {
            let Some(kind) = timer_kind(name) else { return Err(usage()) };
            let ms = if setting.eq_ignore_ascii_case("on") { default_ms(kind) }
                else if setting.eq_ignore_ascii_case("off") { 0 }
                else { duration_ms(setting).ok_or_else(|| slash_error(messages::gm::gm_cwatch_duration_invalid(setting)))? };
            let me = args::selected_character(&c.session)?;
            let snapshot = control::set_timer(&c.db, &c.world, me, TimerChange { kind, duration_ms: ms }).await?;
            Ok(status_text(me, &snapshot))
        }
        _ => Err(usage()),
    }
}

#[cfg(all(test, feature = "tests-slash"))] mod tests;
