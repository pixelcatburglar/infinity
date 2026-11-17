//! Character Crimewatch deadline mutation, expiry workers and flag
//! publishing (`eve/client/script/ui/services/crimewatchSvc.py` timer and
//! flag notifications). This is the single implementation of the pilot
//! deadline mechanism: every write to a pilot's six FILETIME deadlines goes
//! through `write_deadline`, at most one live expiry worker exists per
//! (character, kind), and the criminal/suspect/disapproval flag rosters are
//! re-published from freshly read state after their async audience lookups,
//! never from a pre-query snapshot. Deadlines ride Tokio sleeps that follow
//! pilots across systems and stations; nothing here polls, schedules jobs
//! or survives a restart. The only database use is the flag audience's
//! character location read.

use super::*;
use super::control::TimerKind;
use crate::client::ms_from_filetime_ticks;

/// Existing server grace period after a character timer's expiry.
const CHARACTER_TIMER_GRACE_MS: u64 = 500;

impl TimerKind {
    /// Fixed slot order shared by `Pilot::timer_generations` and status rows.
    pub(super) const ALL: [Self; 6] = [
        Self::Weapons, Self::Pvp, Self::Npc, Self::Criminal, Self::Suspect, Self::Disapproval,
    ];
    /// `Pilot::timer_generations` slot.
    pub(super) fn index(self) -> usize {
        match self {
            Self::Weapons => 0,
            Self::Pvp => 1,
            Self::Npc => 2,
            Self::Criminal => 3,
            Self::Suspect => 4,
            Self::Disapproval => 5,
        }
    }
    /// The pilot's own client timer notification (`crimewatchSvc.On*TimerUpdate`).
    fn event(self) -> &'static str {
        match self {
            Self::Weapons => "OnWeaponsTimerUpdate",
            Self::Pvp => "OnPvpTimerUpdate",
            Self::Npc => "OnNpcTimerUpdate",
            Self::Criminal | Self::Suspect => "OnCriminalTimerUpdate",
            Self::Disapproval => "OnDisapprovalTimerUpdate",
        }
    }
    /// Client running state of the slot (crimewatch.const).
    fn running(self) -> i64 {
        match self {
            Self::Weapons => WEAPONS_TIMER,
            Self::Pvp => PVP_TIMER,
            Self::Npc => NPC_TIMER,
            Self::Criminal => CRIMINAL_TIMER,
            Self::Suspect => SUSPECT_TIMER,
            Self::Disapproval => DISAPPROVAL_TIMER,
        }
    }
    /// Client idle state of the slot (crimewatch.const).
    fn idle(self) -> i64 {
        match self {
            Self::Weapons => WEAPONS_IDLE,
            Self::Pvp => PVP_IDLE,
            Self::Npc => NPC_IDLE,
            Self::Criminal | Self::Suspect => CRIMINAL_IDLE,
            Self::Disapproval => DISAPPROVAL_IDLE,
        }
    }
    /// Kinds whose state also publishes a system flag roster after the async
    /// audience lookup; their pilot notice rides that publish, not the write.
    fn has_flag_audience(self) -> bool {
        matches!(self, Self::Criminal | Self::Suspect | Self::Disapproval)
    }
}

impl Pilot {
    fn until(&self, kind: TimerKind) -> i64 {
        match kind {
            TimerKind::Weapons => self.weapons_until,
            TimerKind::Pvp => self.pvp_until,
            TimerKind::Npc => self.npc_until,
            TimerKind::Criminal => self.criminal_until,
            TimerKind::Suspect => self.suspect_until,
            TimerKind::Disapproval => self.disapproval_until,
        }
    }
    fn set_until(&mut self, kind: TimerKind, deadline: i64) {
        match kind {
            TimerKind::Weapons => self.weapons_until = deadline,
            TimerKind::Pvp => self.pvp_until = deadline,
            TimerKind::Npc => self.npc_until = deadline,
            TimerKind::Criminal => self.criminal_until = deadline,
            TimerKind::Suspect => self.suspect_until = deadline,
            TimerKind::Disapproval => self.disapproval_until = deadline,
        }
    }
}

/// What an expiry worker holding `generation` observes for `kind` at `now`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expiry {
    /// The generation was replaced; exit without publishing.
    Stale,
    /// The live deadline is still ahead; keep sleeping.
    Pending,
    /// The live deadline passed; publish the expiry.
    Due,
}

/// The exact judgement the spawned worker runs on each wake: only the live
/// generation may act, and only once the current deadline has passed.
fn expiry(pilot: &Pilot, kind: TimerKind, generation: u64, now: i64) -> Expiry {
    if pilot.timer_generations[kind.index()] != generation {
        Expiry::Stale
    } else if pilot.until(kind) > now {
        Expiry::Pending
    } else {
        Expiry::Due
    }
}

/// Write one deadline under the shared lock. `refresh` keeps a longer live
/// deadline (`max`); an absolute write replaces it. Extending a live timer
/// keeps the running generation so the existing worker wakes to the later
/// deadline; start, restart after expiry, shorten and off switch the
/// generation so a late worker can never publish for the old deadline.
/// Returns the generation a new expiry worker must run as, or `None`.
pub(super) fn write_deadline(
    st: &mut State, character: i64, kind: TimerKind, deadline: i64, refresh: bool, now: i64,
) -> Option<u64> {
    let pilot = st.pilots.entry(character).or_default();
    let old = pilot.until(kind);
    let deadline = if refresh { old.max(deadline) } else { deadline };
    let generation = &mut pilot.timer_generations[kind.index()];
    let spawn = if deadline > now && (old <= now || deadline < old) {
        // Live start, restart, or a shorter absolute reset.
        *generation = generation.saturating_add(1);
        Some(*generation)
    } else if deadline <= now {
        // Off or an already-past deadline invalidates the running worker;
        // ending a criminal flag also drops the queued CONCORD response in
        // the same critical section, before any publish or await.
        *generation = generation.saturating_add(1);
        if kind == TimerKind::Criminal {
            st.concord.remove(&character);
        }
        None
    } else {
        // Same or later deadline: the live worker re-reads it on wake.
        None
    };
    pilot.set_until(kind, deadline);
    if kind == TimerKind::Disapproval && deadline > now {
        pilot.had_disapproval = true;
    }
    spawn
}

/// The pilot's own timer notice for `kind` as currently stored: running
/// state and deadline, or idle. Criminal and suspect resolve through the
/// shared fourth-slot priority rule.
pub(super) fn notice(st: &State, character: i64, kind: TimerKind, now: i64) -> (&'static str, i64, Option<i64>) {
    match kind {
        TimerKind::Criminal | TimerKind::Suspect => {
            let (_, state, until) = st.pilots.get(&character)
                .map(|p| effective_flag(p, now)).unwrap_or((0, CRIMINAL_IDLE, None));
            ("OnCriminalTimerUpdate", state, until)
        }
        _ => {
            let until = st.pilots.get(&character).map(|p| p.until(kind)).unwrap_or(0);
            if until > now {
                (kind.event(), kind.running(), Some(until))
            } else {
                (kind.event(), kind.idle(), None)
            }
        }
    }
}

/// Write `deadline` for `kind` under the lock and, for kinds without a flag
/// audience, publish the pilot's own timer notice inside the same critical
/// section. Returns the generation a new expiry worker must run as.
fn write_and_notify(
    character: i64, kind: TimerKind, deadline: i64, refresh: bool,
    send: impl FnOnce(i64, &str, Vec<Value>),
) -> Option<u64> {
    let mut st = state();
    let now = filetime_now();
    let spawn = write_deadline(&mut st, character, kind, deadline, refresh, now);
    if !kind.has_flag_audience() {
        let (event, state_code, until) = notice(&st, character, kind, now);
        send(character, event, vec![Value::Int(state_code), until.map(Value::Int).unwrap_or(Value::None)]);
    }
    spawn
}

/// Sync in-lock write and pilot notice for kinds with no flag audience
/// (weapons/PvP refresh and the actor-side NPC contact). Roster kinds go
/// through `set` so their flags publish after the audience lookup.
pub(super) fn activate(world: &Arc<World>, character: i64, kind: TimerKind, deadline: i64, refresh: bool) {
    debug_assert!(!kind.has_flag_audience());
    if let Some(generation) = write_and_notify(character, kind, deadline, refresh,
        |id, event, args| world.notify_pilot(id, event, args)) {
        spawn_expiry(world, character, kind, generation);
    }
}

/// The full service write path shared by services and controls: the pilot
/// notice under the lock for audience-free kinds, the expiry worker when the
/// write starts one, then the flag roster republish for criminal, suspect
/// and disapproval.
pub(super) async fn set(world: &Arc<World>, db: &Db, character: i64, kind: TimerKind, deadline: i64, refresh: bool) {
    if let Some(generation) = write_and_notify(character, kind, deadline, refresh,
        |id, event, args| world.notify_pilot(id, event, args)) {
        spawn_expiry(world, character, kind, generation);
    }
    match kind {
        TimerKind::Criminal | TimerKind::Suspect => announce_flag(db, world, character, None).await,
        TimerKind::Disapproval => announce_disapproval(db, world, character, None).await,
        _ => {}
    }
}

/// Turn every deadline off in one critical section; idle notices for kinds
/// without a flag audience are sent inside it. The caller publishes the
/// criminal/suspect and disapproval rosters through the `announce_*` pass.
pub(super) fn reset(world: &Arc<World>, character: i64) {
    let mut st = state();
    let now = filetime_now();
    for kind in TimerKind::ALL {
        write_deadline(&mut st, character, kind, 0, false, now);
    }
    for kind in TimerKind::ALL {
        if !kind.has_flag_audience() {
            world.notify_pilot(character, kind.event(), vec![Value::Int(kind.idle()), Value::None]);
        }
    }
}

/// The configured natural duration of `kind` as a FILETIME deadline;
/// suspect comes from the kill-right policy, the rest from crimewatch.
pub(super) fn natural_deadline(kind: TimerKind, now: i64) -> i64 {
    let game = crate::config::game();
    let secs = match kind {
        TimerKind::Weapons => game.crimewatch.weapons_timer_secs,
        TimerKind::Pvp => game.crimewatch.pvp_timer_secs,
        TimerKind::Npc => game.crimewatch.npc_timer_secs,
        TimerKind::Criminal => game.crimewatch.criminal_timer_secs,
        TimerKind::Suspect => game.bounty.kill_right_suspect_secs,
        TimerKind::Disapproval => game.crimewatch.disapproval_timer_secs,
    };
    crate::client::filetime_deadline_from_seconds_saturating(now, secs)
}

/// Actor-side natural NPC contact (`combat::damage::complete_damage_outcome`
/// is the only caller): refresh the pilot's NPC timer from `at`, the
/// accepted hit's FILETIME stamp. The running notice goes through the
/// system's own channel; a needed expiry worker upgrades the actor's stored
/// world handle.
pub(crate) fn npc_contact(system: &crate::space::System, character: i64, at: i64) {
    let generation = write_and_notify(character, TimerKind::Npc,
        natural_deadline(TimerKind::Npc, at), true,
        |id, event, args| system.notify_pilot(id, event, args));
    if let Some(generation) = generation {
        if let Some(world) = system.combat_world() {
            spawn_expiry(&world, character, TimerKind::Npc, generation);
        }
    }
}

/// One expiry worker per live (character, kind) deadline. Each wake re-reads
/// the latest deadline under the lock, so an extension keeps sleeping toward
/// the new time while a switched generation exits without publishing.
pub(super) fn spawn_expiry(world: &Arc<World>, character: i64, kind: TimerKind, generation: u64) {
    let world = world.clone();
    tokio::spawn(async move {
        loop {
            let until = state().pilots.get(&character).map(|p| p.until(kind)).unwrap_or(0);
            let wait = ms_from_filetime_ticks(until.saturating_sub(filetime_now()).max(0)).max(0) as u64
                + CHARACTER_TIMER_GRACE_MS;
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
            let status = {
                let st = state();
                st.pilots.get(&character).map_or(Expiry::Stale, |p| expiry(p, kind, generation, filetime_now()))
            };
            match status {
                Expiry::Stale => return,
                Expiry::Pending => continue,
                Expiry::Due => break,
            }
        }
        publish_expired(&world, character, kind, generation).await;
    });
}

/// Publish a due expiry. Flag kinds go through their audience path, which
/// re-reads the current state after the async lookup so a racing write can
/// never be overwritten by a stale idle; other kinds re-check inside the
/// publish lock so their idle stays ordered after the deadline it ends.
async fn publish_expired(world: &Arc<World>, character: i64, kind: TimerKind, generation: u64) {
    match kind {
        TimerKind::Criminal | TimerKind::Suspect => announce_flag(world.db(), world, character, Some((kind, generation))).await,
        TimerKind::Disapproval => announce_disapproval(world.db(), world, character, Some((kind, generation))).await,
        _ => {
            let st = state();
            if st.pilots.get(&character).is_some_and(|p| expiry(p, kind, generation, filetime_now()) == Expiry::Due) {
                world.notify_pilot(character, kind.event(), vec![Value::Int(kind.idle()), Value::None]);
            }
        }
    }
}

/// Publish the pilot's effective criminal/suspect flag: the audience lookup
/// runs first, then the current state is re-read inside the lock so the
/// timer slot and the system roster never carry a stale snapshot. When an
/// `expected` expiry is supplied, the whole publish is skipped unless that
/// generation is still live and due, so a superseded worker emits nothing.
pub(super) async fn announce_flag(db: &Db, world: &World, char_id: i64, expected: Option<(TimerKind, u64)>) {
    let system = match crate::dao::characters::crimewatch::solar_system(db, char_id).await {
        Ok(system) => system,
        Err(e) => { warn!("crimewatch: locating {char_id}: {e}"); None }
    };
    let audience = match system {
        Some(system) => pilots_in(world, system).await,
        None => Vec::new(),
    };
    let st = state();
    let now = filetime_now();
    if let Some((kind, generation)) = expected {
        if st.pilots.get(&char_id).map(|p| expiry(p, kind, generation, now)) != Some(Expiry::Due) {
            return;
        }
    }
    let (slot, timer_state, until) = st.pilots.get(&char_id)
        .map(|p| effective_flag(p, filetime_now()))
        .unwrap_or((0, CRIMINAL_IDLE, None));
    world.notify_pilot(char_id, "OnCriminalTimerUpdate",
        vec![Value::Int(timer_state), until.map(Value::Int).unwrap_or(Value::None)]);
    let mut flags = vec![Value::list([]), Value::list([]), Value::list([])];
    flags[slot] = Value::list([Value::Int(char_id)]);
    for pilot in audience {
        world.notify_pilot(pilot, "OnSystemCriminalFlagUpdates", flags.clone());
    }
}

/// Publish the pilot's disapproval timer and the system's disapproval
/// roster, idle entries first and naughty entries second, matching the
/// client's `OnSystemDisapprovalFlagUpdates(newIdles, newNaughty)` consumer.
/// `expected` expiry is guarded exactly like `announce_flag`.
pub(super) async fn announce_disapproval(db: &Db, world: &World, char_id: i64, expected: Option<(TimerKind, u64)>) {
    let system = match crate::dao::characters::crimewatch::solar_system(db, char_id).await {
        Ok(system) => system,
        Err(e) => { warn!("crimewatch: locating {char_id}: {e}"); None }
    };
    let audience = match system {
        Some(system) => pilots_in(world, system).await,
        None => Vec::new(),
    };
    let st = state();
    let now = filetime_now();
    if let Some((kind, generation)) = expected {
        if st.pilots.get(&char_id).map(|p| expiry(p, kind, generation, now)) != Some(Expiry::Due) {
            return;
        }
    }
    let until = st.pilots.get(&char_id).map(|p| p.disapproval_until).unwrap_or(0);
    let active = until > now;
    world.notify_pilot(char_id, "OnDisapprovalTimerUpdate", vec![
        Value::Int(if active { DISAPPROVAL_TIMER } else { DISAPPROVAL_IDLE }),
        if active { Value::Int(until) } else { Value::None },
    ]);
    let (idle, naughty) = if active { (vec![], vec![char_id]) } else { (vec![char_id], vec![]) };
    let args = vec![
        Value::list(idle.into_iter().map(Value::Int)),
        Value::list(naughty.into_iter().map(Value::Int)),
    ];
    for pilot in audience {
        world.notify_pilot(pilot, "OnSystemDisapprovalFlagUpdates", args.clone());
    }
}

#[cfg(all(test, feature = "tests-combat"))] mod tests;
