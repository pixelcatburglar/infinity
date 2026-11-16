//! Shared Crimewatch control operations: the six character timer writes, the
//! persisted safety level, a full clear, and the self-inflicted `/naughty`
//! crime. Both the ordinary `crimewatch` service (`SetSafetyLevel`,
//! `GetClientStates`) and the GM `/cwatch`/`/naughty` commands consume these;
//! the slash service owns authorization and reply text (`messages::gm`),
//! while ordinary service refusals use `messages::crimewatch`. Timer writes
//! are absolute resets through the shared timer mechanism; safety writes are
//! one guarded update with no push, no SessionChange and no enforcement.
//! `/naughty` admits everything inside a single actor turn (live session,
//! same ship, live membership, deadline writes, pilot notices, flag roster
//! and ticket reservation), so a stale session or a racing clear can never
//! earn partial effects. A criminal `on` writes its deadline first and then
//! admits its CONCORD request in one actor turn that rechecks the flag is
//! still live. `/naughty` creates no victim, engagement, security loss,
//! kill right or killmail.

use super::*;
use crate::client::filetime_ticks_from_ms;
use crate::services::identity::Identity;
use crate::services::notify_error;
use crate::space::combat::CombatPhase;
use infinity_core::session::OnlineSession;

/// One of the six character-level Crimewatch deadlines; order matches the
/// pilot's worker-generation slots and the status row order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerKind {
    Weapons,
    Pvp,
    Npc,
    Criminal,
    Suspect,
    Disapproval,
}

/// The client's safety level enum (crimewatch.const), persisted as an integer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SafetyLevel {
    None = 0,
    Partial = 1,
    Full = 2,
}

/// One absolute timer write from a command: `duration_ms == 0` turns the
/// kind off, a positive value is the new absolute duration, never a delta.
/// Negative values are refused by the caller-facing validation.
pub(crate) struct TimerChange {
    pub kind: TimerKind,
    pub duration_ms: i64,
}

/// The character's live deadlines (`None` when not running) and safety.
pub(crate) struct Snapshot {
    pub weapons: Option<i64>,
    pub pvp: Option<i64>,
    pub npc: Option<i64>,
    pub criminal: Option<i64>,
    pub suspect: Option<i64>,
    pub disapproval: Option<i64>,
    pub safety: SafetyLevel,
}

/// `/naughty` result: the existing response is queued (already or freshly),
/// the request is suppressed (NPC-ignore or disabled), or the pilot is not
/// in high-sec so no criminal timer or response applies.
pub(crate) enum NaughtyOutcome {
    Queued,
    Ignored,
    NotHighsec,
}

impl SafetyLevel {
    /// Map a persisted value back to the enum; the column's CHECK keeps it in
    /// range, so anything else is corrupt data, not a player refusal.
    fn from_stored(level: i64) -> Result<Self, CallError> {
        match level {
            0 => Ok(Self::None),
            1 => Ok(Self::Partial),
            2 => Ok(Self::Full),
            _ => Err(CallError::Internal(anyhow::anyhow!("safety_level {level} outside the 0..=2 enum"))),
        }
    }
}

/// The calling character's row is missing or deleted; a normal refusal,
/// never silently treated as `Full`.
fn unavailable() -> CallError {
    notify_error(messages::crimewatch::CHARACTER_UNAVAILABLE)
}

/// The persisted safety level of one live character; also the character
/// existence check every mutation performs before writing anything.
async fn stored_safety(db: &(impl Executor + ?Sized), character: i64) -> Result<SafetyLevel, CallError> {
    let level = crate::dao::characters::crimewatch::safety(db, character).await.map_err(db_err)?
        .ok_or_else(unavailable)?;
    SafetyLevel::from_stored(level)
}

/// The character's six live deadlines and persisted safety.
pub(crate) async fn snapshot(db: &Db, character: i64) -> Result<Snapshot, CallError> {
    Ok(snapshot_with(stored_safety(db, character).await?, character))
}

/// Snapshot the in-memory deadlines alongside an already-read safety level.
fn snapshot_with(safety: SafetyLevel, character: i64) -> Snapshot {
    let now = filetime_now();
    let st = state();
    let pilot = st.pilots.get(&character);
    let live = |until: i64| (until > now).then_some(until);
    Snapshot {
        weapons: pilot.and_then(|p| live(p.weapons_until)),
        pvp: pilot.and_then(|p| live(p.pvp_until)),
        npc: pilot.and_then(|p| live(p.npc_until)),
        criminal: pilot.and_then(|p| live(p.criminal_until)),
        suspect: pilot.and_then(|p| live(p.suspect_until)),
        disapproval: pilot.and_then(|p| live(p.disapproval_until)),
        safety,
    }
}

/// Apply one absolute timer write after validating the character exists.
/// `duration_ms == 0` turns the kind off and ending a criminal flag cancels
/// the queued response inside the write's own critical section; negative or
/// overflowing durations are refused. A positive criminal duration requests
/// the existing response only when the character really is flying in
/// high-sec with that exact deadline still live.
pub(crate) async fn set_timer(db: &Db, world: &Arc<World>, character: i64, change: TimerChange) -> Result<Snapshot, CallError> {
    let safety = stored_safety(db, character).await?;
    if change.duration_ms < 0 {
        return Err(notify_error(messages::crimewatch::DURATION_INVALID));
    }
    let deadline = if change.duration_ms == 0 {
        0
    } else {
        let ticks = change.duration_ms.checked_mul(filetime_ticks_from_ms(1.0))
            .ok_or_else(|| notify_error(messages::crimewatch::DURATION_INVALID))?;
        filetime_now().checked_add(ticks)
            .ok_or_else(|| notify_error(messages::crimewatch::DURATION_INVALID))?
    };
    timers::set(world, db, character, change.kind, deadline, false).await;
    if change.kind == TimerKind::Criminal && deadline > 0 {
        let generation = {
            let st = state();
            st.pilots.get(&character).map(|p| p.timer_generations[TimerKind::Criminal.index()]).unwrap_or(0)
        };
        request_concord(world, character, generation).await;
    }
    Ok(snapshot_with(safety, character))
}

/// Turn all six timers off, publish their timer and flag updates and cancel
/// a queued CONCORD response. Safety, security status, engagements and kill
/// rights are deliberately preserved.
pub(crate) async fn clear(db: &Db, world: &Arc<World>, character: i64) -> Result<Snapshot, CallError> {
    let safety = stored_safety(db, character).await?;
    // Kill the queued response before any mutation or publish; the criminal
    // write inside `reset` also removes tickets reserved meanwhile, in the
    // same critical section as the deadline resets.
    cancel_concord(character);
    timers::reset(world, character);
    timers::announce_flag(db, world, character, None).await;
    timers::announce_disapproval(db, world, character, None).await;
    Ok(snapshot_with(safety, character))
}

/// Persist one safety level for `character`. This is the only write path,
/// shared by the ordinary bound `SetSafetyLevel` call and `/cwatch safety`.
pub(crate) async fn set_safety(
    ex: &(impl Executor + ?Sized), character: i64, level: SafetyLevel,
) -> Result<SafetyLevel, CallError> {
    let stored = crate::dao::characters::crimewatch::set_safety(ex, character, level as i64)
        .await.map_err(db_err)?.ok_or_else(unavailable)?;
    SafetyLevel::from_stored(stored)
}

/// Actor-side admission shared by `/naughty` and CONCORD requests: `expected`
/// is still the live session, the same ship is still piloted by its owner,
/// and its combat membership is live. Same shape as the admission the
/// `/suicide` path performs, minus the damage-facing incarnation check.
fn live_member(s: &crate::space::System, character: i64, ship: i64, expected: &OnlineSession) -> bool {
    s.combat_session_current(character, Some(expected))
        && s.pilot_ball(character)
            .is_some_and(|ball| ball.id == ship && ball.char_id == Some(character) && ball.owner_id == character)
        && s.persistent_generation(ship).is_some()
        && s.persistent_generation(ship) == s.combat_membership_generation(ship)
        && s.combat_phase(ship) == Some(CombatPhase::Live)
}

/// Refresh one timer from natural policy inside an actor turn, notify the
/// pilot inside the same critical section, and record a new worker
/// generation to spawn afterwards.
fn write_natural(
    s: &crate::space::System, st: &mut State, character: i64, kind: TimerKind,
    spawns: &mut [Option<(TimerKind, u64)>], spawn_count: &mut usize, now: i64,
) {
    let deadline = timers::natural_deadline(kind, now);
    if let Some(generation) = timers::write_deadline(st, character, kind, deadline, true, now) {
        spawns[*spawn_count] = Some((kind, generation));
        *spawn_count += 1;
    }
    let (event, code, until) = timers::notice(st, character, kind, now);
    s.notify_pilot(character, event, vec![Value::Int(code), until.map(Value::Int).unwrap_or(Value::None)]);
}

/// `/naughty`: the caller's own live ship commits a self-inflicted crime.
/// One actor turn re-validates the session, ship and membership and then
/// performs every synchronous effect: weapons and PvP refresh always, and in
/// high-sec the criminal refresh, its flag roster and the ticket reservation,
/// so a stale session cannot split them. The criminal flag goes to the live
/// system audience through the actor's own channel, so no location read is
/// needed.
pub(crate) async fn naughty(
    _db: &Db, world: &Arc<World>, identity: &Identity, expected: &OnlineSession,
) -> Result<NaughtyOutcome, CallError> {
    let character = identity.char_id;
    let Some((ship, system)) = expected.ship_id.zip(expected.system_id) else {
        return Err(notify_error(messages::gm::GM_OVERLAY_SPACE_REQUIRED));
    };
    if !(character > 0
        && character == expected.character_id
        && identity.ship_id == expected.ship_id
        && identity.system_id == expected.system_id
        && identity.station_id.is_none()
        && expected.station_id.is_none())
    {
        return Err(notify_error(messages::gm::GM_OVERLAY_SPACE_REQUIRED));
    }
    let policy = &crate::config::game().crimewatch;
    let security = system_security(world, system);
    let expected = expected.clone();
    let Some((spawns, outcome, ticket)) = world.with(system, move |s| {
        if !live_member(s, character, ship, &expected) { return None; }
        let npc_ignored = s.npc_target_ignored(ship);
        let now = filetime_now();
        let mut st = state();
        let mut spawns = [None; 3];
        let mut spawn_count = 0usize;
        for kind in [TimerKind::Weapons, TimerKind::Pvp] {
            write_natural(s, &mut st, character, kind, &mut spawns, &mut spawn_count, now);
        }
        if security < policy.highsec_threshold {
            return Some((spawns, NaughtyOutcome::NotHighsec, None));
        }
        write_natural(s, &mut st, character, TimerKind::Criminal, &mut spawns, &mut spawn_count, now);
        // The criminal flag roster goes straight to the live system audience.
        let (slot, ..) = st.pilots.get(&character)
            .map(|p| effective_flag(p, now)).unwrap_or((0, CRIMINAL_IDLE, None));
        let mut flags = vec![Value::list([]), Value::list([]), Value::list([])];
        flags[slot] = Value::list([Value::Int(character)]);
        for pilot in s.pilots().filter_map(|b| b.char_id) {
            s.notify_pilot(pilot, "OnSystemCriminalFlagUpdates", flags.clone());
        }
        if npc_ignored || !policy.concord_enabled {
            return Some((spawns, NaughtyOutcome::Ignored, None));
        }
        Some((spawns, NaughtyOutcome::Queued, reserve_concord(&mut st, true, security, policy.highsec_threshold, character).map(PendingConcord)))
    }).await.flatten() else {
        return Err(notify_error(messages::gm::GM_OVERLAY_SPACE_REQUIRED));
    };
    for (kind, generation) in spawns.into_iter().flatten() {
        timers::spawn_expiry(world, character, kind, generation);
    }
    if let Some(pending) = ticket {
        dispatch_concord(world.clone(), system, pending, security);
    }
    Ok(outcome)
}

/// Request the existing CONCORD response for a character really flying in
/// high-sec with the caller-written flag still live under its own
/// generation; the whole admission happens in one actor turn so a racing
/// clear or reset cannot earn a response. Silent when ineligible, superseded
/// or already queued.
async fn request_concord(world: &Arc<World>, character: i64, generation: u64) {
    let Some(session) = world.online_session(character) else { return };
    if session.station_id.is_some() { return; }
    let (ship, system) = match (session.ship_id, session.system_id) {
        (Some(ship), Some(system)) => (ship, system),
        _ => return,
    };
    let policy = &crate::config::game().crimewatch;
    let security = system_security(world, system);
    let ticket = world.with(system, move |s| {
        if !live_member(s, character, ship, &session) || s.npc_target_ignored(ship) {
            return None;
        }
        let mut st = state();
        let live = st.pilots.get(&character).is_some_and(|p|
            p.criminal_until > filetime_now()
                && p.timer_generations[TimerKind::Criminal.index()] == generation);
        live.then(|| reserve_concord(&mut st, policy.concord_enabled, security, policy.highsec_threshold, character).map(PendingConcord))
            .flatten()
    }).await.flatten();
    if let Some(pending) = ticket {
        dispatch_concord(world.clone(), system, pending, security);
    }
}
