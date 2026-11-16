//! GM `/dirt` and `/killmark` commands, forwarded by the slash service:
//! inspect or rewrite a physical hull's persisted appearance through the
//! shared appearance service. A bare command or `status` only reads; `clear`
//! and a ratio or count write. An omitted target freezes the caller's real
//! online session; an explicit positive ship ID accepts any physical hull,
//! including foreign, offline and stored hulls. A write commits before any
//! live slim publication, and no hangar reload or dedicated notification is
//! fabricated.
use super::{args::Args, slash_error, CallError, Client};
use infinity_core::messages;
use infinity_core::session::OnlineSession;
use crate::client::{filetime_now, ms_from_filetime_ticks};
use crate::services::ship::appearance::{dirt, gm, Publication};

fn usage() -> CallError { slash_error(messages::gm::GM_DIRT_USAGE.into()) }
fn invalid() -> CallError { crate::services::notify_error(messages::ship::HULL_APPEARANCE_INACCESSIBLE) }

/// The explicit `shipID` item target, or the caller's frozen current ship.
fn target(c: &Client, args: &[String]) -> Result<gm::Target, CallError> {
    match args.get(1) {
        Some(_) => Ok(gm::Target::Item(Args::new(args).i64(1)?)),
        None => Ok(gm::Target::Current(OnlineSession::from_session(&c.session).ok_or_else(invalid)?)),
    }
}

/// The live-publication outcome text shared by every appearance write.
fn publication(outcome: Publication) -> &'static str {
    match outcome {
        Publication::Published => messages::gm::GM_DIRT_PUBLISHED,
        Publication::NotLoaded => messages::gm::GM_DIRT_NOT_LOADED,
        Publication::Stale => messages::gm::GM_DIRT_STALE,
    }
}

/// `/dirt [<0..1>|status|clear] [shipID]`; a bare command is `status`.
pub(super) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    if args.len() > 2 { return Err(usage()); }
    let parsed = Args::new(args);
    let target = target(c, args)?;
    let now = filetime_now();
    let verb = args.first().map(String::as_str);
    if verb.is_none() || verb.is_some_and(|v| v.eq_ignore_ascii_case("status")) {
        let stored = gm::inspect(&c.db, &c.world, &c.sde, c.login.role, &target).await?;
        let state = match stored.dirt_timestamp {
            Some(stamp) => {
                let setting = dirt::describe(stamp, now)?;
                Some((stamp, ms_from_filetime_ticks(now - stamp).max(0), setting.attained_level, setting.attained_ratio))
            }
            None => None,
        };
        return Ok(messages::gm::gm_dirt_status(stored.hull.item_id, state));
    }
    let requested = if verb.is_some_and(|v| v.eq_ignore_ascii_case("clear")) { 0.0 } else { parsed.f64(0)? };
    let setting = dirt::from_ratio(requested, now).map_err(|_| usage())?;
    let (stored, outcome) = gm::change(&c.db, &c.world, &c.sde, c.login.role, target, gm::Change::Dirt(setting)).await?;
    let stamp = stored.dirt_timestamp
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("committed dirt write returned no timestamp")))?;
    Ok(messages::gm::gm_dirt_changed(stored.hull.item_id, requested, stamp,
        setting.attained_level, setting.attained_ratio, setting.saturated, publication(outcome)))
}

/// `/killmark` and `/killmarks [<count>|status|clear] [shipID]`; a bare
/// command is `status`, `clear` writes 0 and a count is the absolute value.
pub(super) async fn killmarks(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    if args.len() > 2 { return Err(slash_error(messages::gm::GM_KILLMARKS_USAGE.into())); }
    let target = target(c, args)?;
    let verb = args.first().map(String::as_str);
    if verb.is_none() || verb.is_some_and(|v| v.eq_ignore_ascii_case("status")) {
        let stored = gm::inspect(&c.db, &c.world, &c.sde, c.login.role, &target).await?;
        return Ok(messages::gm::gm_killmarks_status(stored.hull.item_id, stored.player_kills));
    }
    let count = if verb.is_some_and(|v| v.eq_ignore_ascii_case("clear")) {
        0
    } else {
        let count = Args::new(args).i64(0)?;
        if count < 0 { return Err(slash_error(messages::gm::GM_KILLMARKS_COUNT_INVALID.into())); }
        count
    };
    let (stored, outcome) = gm::change(&c.db, &c.world, &c.sde, c.login.role, target, gm::Change::Kills(count)).await?;
    Ok(messages::gm::gm_killmarks_changed(stored.hull.item_id, stored.player_kills, publication(outcome)))
}
