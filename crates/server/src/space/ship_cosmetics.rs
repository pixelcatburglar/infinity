//! Actor-held first-party ship SKIN projections for the client ship-state
//! messengers (`shipSkinStateRequestMessenger.py` / `shipSkinStateNoticeMessenger.py`).
//! A projection is prepared once at cold admission and re-prepared inside each
//! mutating transaction, then applied to the actor after commit; the entry path
//! never touches the database or the market. NPC IDs are actor facts, not game
//! item identities, even when their numeric IDs coincide.
use std::collections::HashMap;
use std::sync::Arc;
use infinity_core::consts::{CATEGORY_ENTITY, CATEGORY_SHIP};
use crate::{
    client::{filetime_now, unix_ms_from_filetime_ceil},
    gateway::ship_cosmetics as gateway,
    services::{ship_cosmetics_mgr::state::State, CallError},
    space::{system::observers::{BallToken, DestinyAudience, ObserverToken}, Ball, System, World},
};

/// The persisted ship facts a projection binds to: the item, its hull type,
/// the piloting character and its NPC presentation identity, a live brain,
/// or an explicit admission stored for this live incarnation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ship { pub ship_id: i64, pub type_id: i32, pub character_id: Option<i64>, pub is_npc: bool }

/// The actor fact one live ball projects. NPC presentation identity is a live
/// brain, or an already-installed NPC projection still naming this live
/// incarnation: same token, same id, hull and character. Same-ID successors
/// and stale entries never inherit it.
fn fact(system: &System, ball: &Ball) -> Option<Ship> {
    let entry_npc = system.skins.entries.get(&ball.id).is_some_and(|entry| entry.fact.is_npc
        && system.ball_token_current(entry.token) && entry.token.id() == ball.id
        && entry.fact.ship_id == ball.id && entry.fact.type_id == ball.type_id && entry.fact.character_id == ball.char_id);
    fact_with_npc(system, ball, system.is_npc(ball.id) || entry_npc)
}

/// SKIN eligibility and `Ship` construction for one `is_npc` identity: ship
/// category, or an entity confirmed NPC. `fact` and the one-shot admission
/// bootstrap share it so the category rules exist exactly once.
fn fact_with_npc(system: &System, ball: &Ball, is_npc: bool) -> Option<Ship> {
    let category = system.sde()?.types.get(ball.type_id)?.category_id;
    if category != Some(CATEGORY_SHIP) && !(category == Some(CATEGORY_ENTITY) && is_npc) { return None; }
    Some(Ship { ship_id: ball.id, type_id: ball.type_id, character_id: ball.char_id, is_npc })
}

/// One ship's prepared projection: the actor fact it was computed from, the
/// wire state and the applied license deadline (FILETIME; permanent or no SKIN
/// is `None`). `prepare` guarantees the deadline's `+1` tick is representable.
pub(crate) struct PreparedSkin {
    pub fact: Ship,
    pub state: State,
    pub expires_at: Option<i64>,
}

impl PreparedSkin {
    /// The canonical no_skin projection for one explicitly admitted NPC ball:
    /// the admission itself carries the NPC identity, so no brain is needed.
    /// Pure scalar copy, with no database, market or SDE access.
    pub(in crate::space) fn npc(ball: &Ball) -> Self {
        PreparedSkin {
            fact: Ship { ship_id: ball.id, type_id: ball.type_id, character_id: ball.char_id, is_npc: true },
            state: State { ship_id: ball.id, character_id: 0, skin_id: None },
            expires_at: None,
        }
    }
}

/// Identity of one live ball incarnation on one actor, captured before a
/// transaction so a post-commit projection can never land on a replaced actor
/// or a reinserted same-ID ball.
#[derive(Clone)]
pub(crate) struct SkinFence {
    system: i64,
    actor_gate: std::sync::Weak<tokio::sync::Mutex<()>>,
    token: BallToken,
    fact: Ship,
}
impl SkinFence {
    /// The only builder of a committed change: clones the captured fence and
    /// moves the prepared projection. It does not query World or the database,
    /// allocate a version or send anything; the original fence stays usable for
    /// a transaction retry.
    pub(crate) fn committed(&self, prepared: PreparedSkin) -> CommittedSkin {
        CommittedSkin { fence: self.clone(), prepared }
    }
    /// The actor fact captured at fence time; services build a committed
    /// prepared state from it.
    pub(crate) fn fact(&self) -> &Ship { &self.fact }
}

/// A prepared projection bound to the fence it must apply on.
pub(crate) struct CommittedSkin { fence: SkinFence, prepared: PreparedSkin }

/// Capture the actor incarnation a later committed projection may apply on.
/// `None` means no live actor or no SKIN-eligible ball; the caller keeps its
/// persistent change without forcing a system load.
pub(crate) async fn capture(world: &World, system: i64, ship: i64) -> Option<SkinFence> {
    world.with(system, move |s| {
        let token = s.capture_ball(ship)?;
        let fact = s.ball(ship).and_then(|ball| fact(s, ball))?;
        let actor_gate = s.snapshot_context.as_ref().map(|context| Arc::downgrade(&context.gate))?;
        Some(SkinFence { system, actor_gate, token, fact })
    }).await.flatten()
}

/// Post-commit delivery of one prepared projection: applied only on the
/// captured actor. A rolled-back transaction never reaches this path.
pub(crate) fn publish(world: &World, change: CommittedSkin) {
    world.post(change.fence.system, move |s| { s.apply_skin(change); });
}

/// The stored projection for one ball.
struct SkinEntry {
    token: BallToken,
    fact: Ship,
    state: State,
    expires_at: Option<i64>,
    version: u64,
}
impl SkinEntry {
    /// The state a reader may present at `now` (FILETIME): an overdue deadline
    /// reads as no SKIN, even before the actor timer runs.
    fn public_state(&self, now: i64) -> State {
        if self.expires_at.is_some_and(|deadline| deadline < now) {
            return State { ship_id: self.fact.ship_id, character_id: self.fact.character_id.unwrap_or(0), skin_id: None };
        }
        self.state.clone()
    }
}

/// The actor's ship SKIN projections, one per ball. Written only by
/// `install_skin` and `apply_skin`; read by scene presentation and in-space
/// `GetAll`/`Get` requests. There is no second store.
#[derive(Default)]
pub(in crate::space) struct Skins {
    entries: HashMap<i64, SkinEntry>,
    version: u64,
}
impl Skins {
    /// Forget a departing ball's projection; queued publications revalidate the
    /// token and stale timers revalidate the version, so nothing else is needed.
    pub(in crate::space) fn remove(&mut self, ship: i64) {
        self.entries.remove(&ship);
    }
    /// Store one projection under a fresh checked version.
    fn install(&mut self, token: BallToken, prepared: PreparedSkin) -> Option<u64> {
        self.version = self.version.checked_add(1)?;
        let version = self.version;
        self.entries.insert(prepared.fact.ship_id, SkinEntry {
            token, fact: prepared.fact, state: prepared.state, expires_at: prepared.expires_at, version,
        });
        Some(version)
    }
}

impl System {
    /// Store one prepared projection for a ball the caller already admitted.
    /// Storage only: the real Add action publishes the birth state. An
    /// explicit NPC admission may carry its identity once, when nothing yet
    /// proves it; every regular projection still needs the exact live fact.
    pub(crate) fn install_skin(&mut self, ball: BallToken, prepared: PreparedSkin) -> bool {
        self.store_skin(ball, prepared, true).is_some()
    }

    /// Apply one committed projection: the captured fence must still name this
    /// actor, token and fact. The store and the immediate publication run on
    /// the same actor turn.
    pub(crate) fn apply_skin(&mut self, change: CommittedSkin) -> bool {
        let CommittedSkin { fence, prepared } = change;
        let ship = fence.fact.ship_id;
        let Some(gate) = fence.actor_gate.upgrade() else { return false };
        if fence.system != self.id || fence.fact != prepared.fact
            || !self.snapshot_context.as_ref().is_some_and(|context| Arc::ptr_eq(&context.gate, &gate)) {
            return false;
        }
        let Some(version) = self.store_skin(fence.token, prepared, false) else { return false };
        self.emit_skin(ship, version);
        true
    }

    /// Store a validated projection and arm its expiry. `None` when the ball
    /// token or the live fact no longer matches, or the deadline cannot be
    /// represented; nothing is stored or scheduled on failure.
    fn store_skin(&mut self, ball: BallToken, prepared: PreparedSkin, allow_npc_bootstrap: bool) -> Option<u64> {
        let ship = prepared.fact.ship_id;
        if ball.id() != ship || !self.ball_token_current(ball) { return None; }
        // NPC projections are canonical no_skin for their whole lifetime; a
        // non-canonical prepared is refused before any fact check or store.
        if prepared.fact.is_npc
            && (prepared.expires_at.is_some() || prepared.state != State { ship_id: ship, character_id: 0, skin_id: None }) {
            return None;
        }
        if self.ball(ship).and_then(|b| fact(self, b)).as_ref() != Some(&prepared.fact) {
            // The first explicit NPC admission of this live incarnation has no
            // brain and no installed projection to prove is_npc yet. Only this
            // exact token may bootstrap that identity; committed applies and
            // existing projections never reclassify through it.
            if !(allow_npc_bootstrap
                && prepared.fact.is_npc
                && self.skins.entries.get(&ship).map_or(true, |entry| entry.token != ball)
                && self.ball(ship).and_then(|b| fact_with_npc(self, b, true)).as_ref() == Some(&prepared.fact)) {
                return None;
            }
        }
        let arm = match prepared.expires_at {
            None => None,
            Some(deadline) => Some((deadline, unix_ms_from_filetime_ceil(deadline.checked_add(1)?))),
        };
        let version = self.skins.install(ball, prepared)?;
        if let Some((deadline, due)) = arm {
            self.schedule_at(due, move |s| s.expire_skin(ship, version, deadline));
        }
        Some(version)
    }

    /// Actor-side deadline of one stored projection. Only the matching version
    /// and deadline apply: a renewed or replaced projection's stale timer
    /// passes without touching it. The actor publishes the no_skin state; the
    /// durable license and jobs stay with the scheduler.
    fn expire_skin(&mut self, ship: i64, version: u64, deadline: i64) {
        let Some(entry) = self.skins.entries.get_mut(&ship) else { return };
        if entry.version != version || entry.expires_at != Some(deadline) { return; }
        let Some(next) = self.skins.version.checked_add(1) else { return };
        self.skins.version = next;
        entry.version = next;
        entry.expires_at = None;
        entry.state = State { ship_id: ship, character_id: entry.fact.character_id.unwrap_or(0), skin_id: None };
        self.emit_skin(ship, next);
    }

    /// Revalidated synchronous fanout of a stored projection to scene observers
    /// who can see the ball: each observer's pending Destiny flushes into its
    /// sender first, then the notice goes through the system's notice hub.
    /// `version` must match or nothing publishes.
    fn emit_skin(&mut self, ship: i64, version: u64) {
        let Some((token, state)) = self.skins.entries.get(&ship)
            .filter(|entry| entry.version == version)
            .map(|entry| (entry.token, entry.public_state(filetime_now()))) else { return };
        if !self.ball_token_current(token) { return; }
        let cloak = self.ball(ship).map_or(0, |ball| ball.cloak);
        let mut characters = Vec::new();
        for (observer, _) in self.scene_observers(&DestinyAudience::Visible { id: ship, cloak }) {
            if !self.flush_observer_pending(observer) { continue; }
            if let Some(character) = self.observer_character(observer) { characters.push(character); }
        }
        let notices = match gateway::notice(&state, &characters) {
            Ok(notices) => notices,
            Err(error) => { tracing::warn!(?error, "ship SKIN notice build failed"); return; }
        };
        for notice in notices { self.dispatch_notice(notice); }
    }

    /// Present stored projections to one observer on their ordered channel:
    /// every projection visible to them on initial entry (`entered` `None`),
    /// or just the ball that became visible. The observer's pending Destiny
    /// flushes into its sender before the notice is enqueued.
    pub(in crate::space) fn present_skins(&mut self, observer: ObserverToken, entered: Option<BallToken>) {
        let Some(character) = self.observer_character(observer) else { return; };
        match entered {
            None => {
                let now = filetime_now();
                let states: Vec<State> = self.skins.entries.values()
                    .filter(|entry| self.ball_token_current(entry.token) && self.scene_has(observer, entry.token))
                    .map(|entry| entry.public_state(now))
                    .collect();
                if !self.flush_observer_pending(observer) { return; }
                match gateway::all_notice(&states, character) {
                    Ok(notice) => self.dispatch_notice(notice),
                    Err(error) => tracing::warn!(?error, "ship SKIN snapshot build failed"),
                }
            }
            Some(ball) => {
                let ship = ball.id();
                if !self.scene_has(observer, ball) { return; }
                let Some(state) = self.skins.entries.get(&ship).map(|entry| entry.public_state(filetime_now())) else { return; };
                if !self.flush_observer_pending(observer) { return; }
                match gateway::notice(&state, &[character]) {
                    Ok(notices) => for notice in notices { self.dispatch_notice(notice); },
                    Err(error) => tracing::warn!(?error, "ship SKIN notice build failed"),
                }
            }
        }
    }

    /// Enqueue one built notice through the system's notice hub; session
    /// resolution stays with the shared pilot registry.
    fn dispatch_notice(&self, notice: crate::gateway::notices::GatewayNotice) {
        if let Some(hub) = &self.notice_hub {
            let registry = self.registry.clone();
            hub.dispatch(notice, move |id| registry.online_session(id));
        }
    }
}

/// In-space `GetAll`: the actor's projections for ships the character may see.
/// No database or market fallback; an unavailable actor is an internal error,
/// never an empty scene.
pub(crate) async fn visible_states(world: &World, system: i64, character: i64) -> Result<Vec<State>, CallError> {
    world.with(system, move |s| {
        let now = filetime_now();
        s.skins.entries.values()
            .filter(|entry| s.fleet_broadcast_visible(character, entry.fact.ship_id))
            .map(|entry| entry.public_state(now))
            .collect()
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship SKIN system actor unavailable")))
}

/// In-space `Get`: the actor's projection for one visible ship, when presented.
/// An unavailable actor is an internal error; an out-of-scope ship is `None`.
pub(crate) async fn projected_state(world: &World, system: i64, character: i64, ship: i64) -> Result<Option<State>, CallError> {
    world.with(system, move |s| {
        if !s.fleet_broadcast_visible(character, ship) { return None; }
        s.skins.entries.get(&ship).map(|entry| entry.public_state(filetime_now()))
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship SKIN system actor unavailable")))
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
