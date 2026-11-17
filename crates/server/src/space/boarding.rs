//! Ship motion authorization and condition-checkpoint settlements for boarding,
//! docking and transfer. State comes from game items and the immutable SDE.
//! Actor transition tokens fence concurrent combat and session replacement.
use super::*;
use std::future::Future;
use infinity_core::consts::CATEGORY_SHIP;
use crate::dao::items::loading::{Item, ItemQuery};
use crate::dao::items::space_ships::FreeShip;
use crate::services::{crimewatch, ship::balls::{self, PreparedShip}, ship::transport::station, CallError};

#[derive(Clone)]
pub(crate) struct BoardSnapshot {
    pub old: Ball,
    pub target: Ball,
    pub old_generation: u64,
    pub target_generation: u64,
}

/// Loaded seeds carrying the committed boundary's captured absolute onto a
/// record the publication just installed; the prepared fit was read before
/// the checkpoint settled, so its durable view is older than these rows.
fn settled_loaded_seeds(s: &System, ship: i64, loaded: &[crate::dao::items::charges::LoadedWrite]) -> Vec<crate::space::combat::loaded::LoadedSeed> {
    loaded.iter().map(|write| crate::space::combat::loaded::LoadedSeed {
        module: write.module, flag: write.flag, type_id: Some(write.type_id), quantity: write.quantity,
        pending: s.actor_combat.get(&ship).and_then(|record| record.loaded.get(&write.module))
            .is_some_and(|slot| slot.pending),
    }).collect()
}

#[path = "boarding/transitions.rs"]
pub(crate) mod transitions;

#[derive(Debug, PartialEq)]
pub(crate) enum BoardRefusal { StalePilot, InvalidTarget, Occupied, Unsafe, OutOfRange }

/// One slot claimed by a boundary hold: the receipt records the owning
/// record's membership plus the slot's flag, incarnation and prior dirty
/// flag; dispositions touch it only while that identity still matches.
struct HeldSlot { ship: i64, membership: u64, module: i64, flag: i32, incarnation: u64, dirty: bool }

/// Why a boundary hold on a ship's loaded slots was refused: transient
/// contention maps to the caller's own busy refusal; a stopped actor is an
/// invariant violation and stays an internal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadedQuiesceRefusal { Busy, Stopped }

impl LoadedQuiesceRefusal {
    /// `busy` is the root's own normal refusal; only a stopped actor becomes
    /// an internal error here.
    pub(crate) fn into_call_error(self, busy: CallError) -> CallError {
        match self {
            Self::Busy => busy,
            Self::Stopped => CallError::Internal(anyhow::anyhow!("system actor stopped during loaded hold")),
        }
    }
}

/// A business hold on a system's loaded slots: dropping it posts the
/// selective disposition — a rollback release before the transaction may
/// have run, an invalidation after. Both are no-ops on a committed or
/// replaced record, and an explicit disposition drains the receipt first.
pub(crate) struct HeldLoaded {
    world: std::sync::Weak<World>, system: i64, slots: Vec<HeldSlot>, in_flight: bool,
}

fn each_held(s: &mut System, slots: &[HeldSlot], mut f: impl FnMut(&HeldSlot, &mut crate::space::combat::loaded::LoadedSlot)) {
    for held in slots {
        let Some(record) = s.actor_combat.get_mut(&held.ship) else { continue };
        if record.membership_generation != held.membership { continue; }
        if let Some(slot) = record.loaded.get_mut(&held.module) {
            if slot.flag == held.flag && slot.incarnation == held.incarnation && slot.held && !slot.unknown {
                f(held, slot);
            }
        }
    }
}

impl Drop for HeldLoaded {
    fn drop(&mut self) {
        if self.slots.is_empty() { return; }
        if let Some(world) = self.world.upgrade() {
            let slots = std::mem::take(&mut self.slots);
            let in_flight = self.in_flight;
            world.post(self.system, move |s| {
                if in_flight {
                    each_held(s, &slots, |_, slot| slot.unknown = true);
                } else {
                    each_held(s, &slots, |held, slot| { slot.held = false; slot.dirty = held.dirty; });
                }
                s.refresh_loaded_counter();
            });
        }
    }
}

impl HeldLoaded {
    /// Cancellation state armed right before the business transaction may
    /// execute: from then on an undelivered disposition invalidates the held
    /// absolutes for a fresh reseed instead of restoring stale flags.
    pub(crate) fn may_execute(&mut self) { self.in_flight = true; }

    /// The membership generation this receipt claimed scope on when the ship
    /// was resident at claim time; `None` means no resident scope was claimed.
    pub(crate) fn claimed_membership(&self) -> Option<u64> { self.slots.first().map(|slot| slot.membership) }


    /// Known commit: publication already ran; unhold only the slots this
    /// receipt still owns and keep the state the save just cleared.
    pub(crate) fn release_committed(&mut self, s: &mut System) {
        let slots = std::mem::take(&mut self.slots);
        each_held(s, &slots, |_, slot| slot.held = false);
        s.refresh_loaded_counter();
    }

    /// Unknown outcome: mark every still-owned slot held and unknown until a
    /// real reseed; returns the scope that was actually invalidated.
    pub(crate) fn invalidate(&mut self, s: &mut System) -> Vec<crate::dao::items::charges::LoadedSlotKey> {
        let slots = std::mem::take(&mut self.slots);
        let mut keys = Vec::new();
        each_held(s, &slots, |held, slot| {
            slot.unknown = true;
            keys.push(crate::dao::items::charges::LoadedSlotKey { ship: held.ship, module: held.module, flag: held.flag });
        });
        s.refresh_loaded_counter();
        keys
    }
}

/// Refuse new loaded use on `ships`' resident slots — including durable
/// pending reservations — then wait out admitted finite work. A ship without
/// a resident record adds no scope; a competing held or unknown hold, a
/// quiescing system or an early drain end is [`LoadedQuiesceRefusal::Busy`],
/// never a cold success; a stopped actor is [`LoadedQuiesceRefusal::Stopped`].
pub(crate) async fn quiesce_ship_loaded(world: &World, system: i64, ships: &[i64]) -> Result<HeldLoaded, LoadedQuiesceRefusal> {
    let mut ships = ships.to_vec();
    ships.sort_unstable();
    ships.dedup();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let claimed = world.with(system, {
        let ships = ships.clone();
        move |s| {
            if s.loaded_quiescing { return Some(Err(())); }
            for &ship in &ships {
                let Some(record) = s.actor_combat.get(&ship) else { continue };
                if record.loaded.values().any(|slot| slot.held || slot.unknown) { return Some(Err(())); }
            }
            let mut slots = Vec::new();
            for &ship in &ships {
                if let Some(record) = s.actor_combat.get_mut(&ship) {
                    let membership = record.membership_generation;
                    for slot in record.loaded.values_mut() {
                        slot.held = true;
                        slots.push(HeldSlot { ship, membership, module: slot.module,
                            flag: slot.flag, incarnation: slot.incarnation, dirty: slot.dirty });
                    }
                }
            }
            s.drain_loaded(tx);
            Some(Ok((s.world.clone(), slots)))
        }
    }).await;
    let (weak, slots) = match claimed {
        Some(Some(Ok(claimed))) => claimed,
        Some(Some(Err(()))) | Some(None) => return Err(LoadedQuiesceRefusal::Busy),
        None => return Err(LoadedQuiesceRefusal::Stopped),
    };
    // The receipt owner exists before the drain wait starts: a caller
    // cancelled here drops it and releases the claim instead of stranding
    // held slots.
    let guard = HeldLoaded { world: weak, system, slots, in_flight: false };
    if rx.recv().await.is_none() {
        return Err(LoadedQuiesceRefusal::Busy);
    }
    Ok(guard)
}

/// Cancels reservations even when the requesting future is dropped.
pub(crate) struct CombatTransition {
    world: std::sync::Weak<World>,
    system: i64,
    pub(crate) entries: Vec<(u64, crate::space::combat::ActorConditionSnapshot)>,
    pub(crate) facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
    /// The business hold this transition's owned worker carries; the root
    /// attaches it after claim so it moves into the deferred job with the
    /// reservation instead of staying a caller-local.
    pub(crate) held_loaded: Option<HeldLoaded>,
    scope: Option<(i64, crate::space::combat_facts::boundaries::Boundary)>,
    armed: bool,
}

impl Drop for CombatTransition {
    fn drop(&mut self) {
        if !self.armed { return; }
        if let Some(world) = self.world.upgrade() {
            let entries: Vec<_> = self.entries.iter().map(|(token, snapshot)|
                (snapshot.item_id, *token, snapshot.generation)).collect();
            world.post(self.system, move |s| {
                for (id, token, generation) in entries {
                    s.cancel_combat_transition(id, token, generation);
                }
            });
        }
    }
}

impl CombatTransition {
    pub(crate) fn disarm(&mut self) { self.armed = false; }
    pub(crate) fn world(&self) -> Option<Arc<World>> { self.world.upgrade() }
}

impl System {
    pub(crate) fn begin_condition_transitions(&mut self, ids: &[i64]) -> Option<CombatTransition> {
        let mut entries: Vec<(u64, crate::space::combat::ActorConditionSnapshot)> = Vec::with_capacity(ids.len());
        for &id in ids {
            let Some((token, snapshot)) = self.begin_combat_transition(id) else {
                for (token, snapshot) in entries {
                    self.cancel_combat_transition(snapshot.item_id, token, snapshot.generation);
                }
                return None;
            };
            entries.push((token, snapshot));
        }
        let facts = match crate::space::snapshot::capture_fact_checkpoint(self) {
            Ok(facts) => facts,
            Err(error) => {
                tracing::warn!(?error, system = self.id, "combat facts checkpoint refused");
                for (token, snapshot) in entries {
                    self.cancel_combat_transition(snapshot.item_id, token, snapshot.generation);
                }
                return None;
            }
        };
        let scope = ids.first().and_then(|id| self.ball(*id)).and_then(|ball| ball.char_id)
            .and_then(|character| self.combat_character_scope(character).map(|scope| (character, scope)));
        Some(CombatTransition { world: self.world.clone(), system: self.id, entries, facts, held_loaded: None, scope, armed: true })
    }

    /// Submit an admitted transition without coupling its lifetime to the receipt.
    /// A closed worker restores every reservation in this actor turn.
    pub(crate) fn defer_condition_transition<F>(
        &mut self, mut transition: CombatTransition,
        make: impl FnOnce(CombatTransition) -> F,
    ) -> bool
    where F: Future<Output = ()> + Send + 'static {
        let reservations: Vec<_> = transition.entries.iter().map(|(token, state)|
            (state.item_id, *token, state.generation)).collect();
        transition.disarm();
        let job = make(transition);
        if self.deferred_jobs.as_ref().is_some_and(|tx| tx.send(Box::pin(job)).is_ok()) {
            true
        } else {
            for (ship, token, generation) in reservations {
                self.cancel_combat_transition(ship, token, generation);
            }
            false
        }
    }
}

async fn retire_stale_transition(world: &World, system: i64, character: i64, ship: i64,
    token: u64, generation: u64, expected: Option<infinity_core::session::OnlineSession>) -> bool {
    world.with(system, move |s| {
        if s.combat_session_current(character, expected.as_ref())
            || s.combat_membership_generation(ship) != Some(generation)
            || s.combat_phase(ship) != Some(crate::space::combat::CombatPhase::Transition { token })
            || (s.contains(ship) && s.persistent_generation(ship) != Some(generation)) {
            return false;
        }
        if s.contains(ship) { s.remove_ball(ship); }
        s.commit_combat_transition(ship, token)
    }).await.unwrap_or(false)
}

async fn retire_authoritative_departure(world: &World, system: i64, ship: i64, token: u64, generation: u64) -> bool {
    world.with(system, move |s| {
        if s.combat_membership_generation(ship) != Some(generation)
            || s.combat_phase(ship) != Some(crate::space::combat::CombatPhase::Transition { token })
            || (s.contains(ship) && s.persistent_generation(ship) != Some(generation)) { return false; }
        if s.contains(ship) { s.remove_ball(ship); }
        s.commit_combat_transition(ship, token)
    }).await.unwrap_or(false)
}

async fn fact_receipt(db: &Db, character: i64, ship: i64, facts: &Option<Arc<crate::space::snapshot::FactCheckpoint>>) -> Result<(Option<bool>, std::collections::HashSet<i64>), crate::db::Error> {
    let Some(facts) = facts.as_ref().map(Arc::clone) else { return Ok((None, Default::default())) };
    db.transaction(|tx, _: &mut ()| {
        let facts = Arc::clone(&facts);
        Box::pin(async move {
            if character != 0 { crate::dao::characters::selection::lock_character(tx, character).await?; }
            crate::dao::items::ship::lock_reload_ship(tx, ship).await?;
            let committed = facts.is_committed(tx).await?;
            let written = if committed { facts.verified_condition_ids(tx).await? } else { Default::default() };
            Ok((Some(committed), written))
        })
    }, |result, ()| async move { Ok(result) }).await
}

async fn acknowledge_facts(world: &World, system: i64, facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>, written: std::collections::HashSet<i64>) {
    if let Some(facts) = facts { let _ = world.with(system, move |s| facts.acknowledge(s, &written)).await; }
}
/// One pending cycle's unknown-ready gate: the failed boundary's narrow
/// fresh must reseed the ship's still-unknown scope before any business
/// conclusion; a failure keeps the hold and releases the pending claim for
/// the next period.
pub(crate) async fn pending_loaded_retry(world: &World, db: &Db, system: i64, ship: i64, token: u64, generation: u64) -> bool {
    if crate::services::combat::loaded::fresh_gate(db, world, system, &[ship]).await { return true; }
    world.with(system, move |s| s.release_pending_combat(ship, token, generation)).await;
    false
}


struct DockSettlement {
    world: std::sync::Weak<World>,
    db: Db,
    character: i64,
    ship: i64,
    source_system: i64,
    destination: station::Destination,
    token: u64,
    generation: u64,
    facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
    /// This character's captured source bucket writer riding the same
    /// transaction; the receipt stays in actor state until proven.
    bounties: Option<Arc<crate::services::bounty::npc::checkpoint::Checkpoint>>,
    expected: Option<infinity_core::session::OnlineSession>,
    admission: crate::space::containment::ActorAdmission,
    receipt: Mutex<Option<tokio::sync::oneshot::Sender<Result<Item, CallError>>>>,
}

impl DockSettlement {
    fn complete_receipt(&self, result: Result<Item, CallError>) {
        if let Some(sender) = crate::poison::lock_or_exit(self.receipt.lock()).take() { let _ = sender.send(result); }
    }

    async fn publish(&self, world: &World) -> bool {
        let (ship, token, generation) = (self.ship, self.token, self.generation);
        let character = self.character;
        let expected = self.expected.clone();
        let removed = world.with(self.source_system, move |s| {
            (character == 0 || s.combat_session_current(character, expected.as_ref()))
                && s.combat_membership_generation(ship) == Some(generation)
                && s.persistent_generation(ship) == Some(generation)
                && s.commit_combat_transition(ship, token) && s.remove_ball(ship).is_some()
        }).await.unwrap_or(false);
        if !removed { return false; }
        if self.character != 0 {
            crate::services::combat::reset_pilot(world, self.character, Some((self.source_system, self.ship)));
            let mut next = self.expected.clone();
            if let Some(session) = &mut next {
                session.station_id = Some(self.destination.station_id);
                session.system_id = Some(self.destination.system_id);
            }
            if !world.replace_online_session(self.character, self.expected.as_ref(), next) { return false; }
        }
        true
    }

    /// Proof of the committed dock: the character's station/system and active
    /// ship plus the hull's owner, root location, flag, assembled singleton
    /// and quantity, not the hull location alone. The already-read root row is
    /// enriched through the shared item loader. `Some(None)` means the reads
    /// succeeded and the committed state is absent.
    async fn committed_dock(&self, sde: &crate::sde::Sde) -> Option<Option<Item>> {
        let rows = crate::services::items::load_items(&self.db, sde, ItemQuery::Id(self.ship)).await;
        let hull = match rows {
            Ok(mut rows) => rows.drain(..).next().filter(|item|
                item.location_id == self.destination.station_id
                    && item.flag_id == infinity_core::consts::FLAG_HANGAR
                    && item.category_id == i64::from(CATEGORY_SHIP)
                    && (self.character == 0 || item.owner_id == self.character)
                    && item.singleton && item.quantity == 1),
            Err(_) => return None,
        };
        if self.character == 0 { return Some(hull); }
        match crate::dao::characters::ship::current(&self.db, self.character).await {
            Ok(Some(location)) if location.ship == Some(self.ship)
                && location.station == Some(self.destination.station_id)
                && location.system == Some(self.destination.system_id) => Some(hull),
            Ok(_) => Some(None),
            Err(_) => None,
        }
    }
    async fn reconcile(self: Arc<Self>) {
        let Some(world) = self.world.upgrade() else { return };
        if self.admission.check().is_err() {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!(crate::space::containment::ActorRetiring))));
            return;
        }
        let market = crate::services::market::lock().await;
        if self.admission.check().is_err() {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!(crate::space::containment::ActorRetiring))));
            return;
        }
        let sde = world.sde().clone();
        let Ok(gate) = world.lock_condition_snapshot(self.source_system).await else {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            world.with(self.source_system, move |s| s.release_pending_combat(ship, token, generation)).await;
            return;
        };
        if !pending_loaded_retry(&world, &self.db, self.source_system, self.ship, self.token, self.generation).await { return; }
        let (receipt, verified) = match fact_receipt(&self.db, self.character, self.ship, &self.facts).await {
            Ok(receipt) => receipt,
            Err(_) => {
                let (ship, token, generation) = (self.ship, self.token, self.generation);
                world.with(self.source_system, move |s| s.release_pending_combat(ship, token, generation)).await;
                return;
            }
        };
        // The bounty receipt proves itself under its own row-lock fence; only a
        // matching business+bounty proof may publish or cancel.
        let bounty_outcome = match &self.bounties {
            Some(receipt) => match crate::services::bounty::npc::resolve_receipt(&self.db, receipt).await {
                Ok(outcome) => {
                    let _ = receipt.settle(&world, outcome).await;
                    Some(outcome)
                }
                Err(_) => None,
            },
            None => None,
        };
        let bounty_durable = self.bounties.is_none() || bounty_outcome == Some(crate::services::bounty::npc::checkpoint::CheckpointOutcome::Durable);
        let bounty_unchanged = self.bounties.is_none() || bounty_outcome == Some(crate::services::bounty::npc::checkpoint::CheckpointOutcome::Unchanged);
        let committed = self.committed_dock(&sde).await;
        match committed {
            Some(Some(hull)) if receipt != Some(false) && bounty_durable => {
                if receipt == Some(true) {
                    acknowledge_facts(&world, self.source_system, self.facts.clone(), verified).await;
                }
                if self.character != 0
                    && retire_stale_transition(&world, self.source_system, self.character, self.ship, self.token, self.generation, self.expected.clone()).await {
                    self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("dock session changed after commit"))));
                    return;
                }
                if self.publish(&world).await {
                    drop(gate);
                    drop(market);
                    if self.character != 0 {
                        if let Err(error) = crate::services::corps::pending_kicks::trigger(&self.db, &world, self.character).await {
                            tracing::warn!(?error, character = self.character, "completed dock pending kick failed");
                        }
                    }
                    self.complete_receipt(Ok(hull));
                    return;
                }
            }
            Some(None) => {
                // A read failure stays unknown and keeps the pending protocol;
                // it never counts as confirmed source or destination.
                let at_source = if self.character == 0 {
                    crate::dao::items::loading::load_items(&self.db, ItemQuery::Id(self.ship)).await
                        .map(|rows| rows.iter().any(|row| row.location_id == self.source_system)).ok()
                } else {
                    crate::dao::combat::victim_current(&self.db, self.character, self.ship, self.source_system).await.ok()
                };
                match at_source {
                    // The rollback proof only cancels when the bounty receipt
                    // also proves it never wrote; anything else stays pending.
                    Some(true) if receipt != Some(true) && bounty_unchanged => {
                        let (ship, token, generation) = (self.ship, self.token, self.generation);
                        let restored = world.with(self.source_system, move |s| s.cancel_combat_transition(ship, token, generation)).await.unwrap_or(false);
                        if restored {
                            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("dock transaction did not commit"))));
                            return;
                        }
                    }
                    Some(false) => {
                        if retire_authoritative_departure(&world, self.source_system, self.ship, self.token, self.generation).await {
                            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship moved during dock reconciliation"))));
                            return;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        let (ship, token, generation) = (self.ship, self.token, self.generation);
        world.with(self.source_system, move |s| s.release_pending_combat(ship, token, generation)).await;
    }

    fn callback(self: &Arc<Self>) -> Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> {
        let plan = self.clone();
        Arc::new(move || {
            let plan = plan.clone();
            Box::pin(async move { plan.reconcile().await })
        })
    }
}


struct TransferSettlement {
    world: std::sync::Weak<World>,
    db: Db,
    character: i64,
    ship: i64,
    source: i64,
    dest: i64,
    original: Ball,
    position: V3,
    prior_position: Mutex<Option<V3>>,
    token: u64,
    generation: u64,
    snapshot: crate::space::combat::ActorConditionSnapshot,
    facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
    /// The character's captured source bucket writer riding the same
    /// transaction; the receipt stays in actor state until proven.
    bounties: Option<Arc<crate::services::bounty::npc::checkpoint::Checkpoint>>,
    scope: Option<crate::space::combat_facts::boundaries::Boundary>,
    expected: Option<infinity_core::session::OnlineSession>,
    cloak_until: Option<i64>,
    admission: (crate::space::containment::ActorAdmission, Option<crate::space::containment::ActorAdmission>),
    receipt: Mutex<Option<tokio::sync::oneshot::Sender<Result<(), CallError>>>>,
}

impl TransferSettlement {
    fn callback(self: &Arc<Self>) -> Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> {
        let plan = self.clone();
        Arc::new(move || {
            let plan = plan.clone();
            Box::pin(async move { plan.reconcile().await })
        })
    }

    fn complete_receipt(&self, result: Result<(), CallError>) {
        if let Some(sender) = crate::poison::lock_or_exit(self.receipt.lock()).take() { let _ = sender.send(result); }
    }

    async fn publish(&self, world: &World, prepared: PreparedShip, snapshot: crate::space::combat::ActorConditionSnapshot,
        settled: Vec<crate::dao::items::charges::LoadedWrite>) -> bool {
        if !prepared.valid_transfer(&snapshot) { return false; }
        let (ship, token, generation) = (self.ship, self.token, self.generation);
        let expected = self.expected.clone();
        let character = self.character;
        if self.source == self.dest {
            let installed = world.with(self.source, move |s| {
                s.combat_session_current(character, expected.as_ref())
                    && s.combat_membership_generation(ship) == Some(generation)
                    && s.persistent_generation(ship) == Some(generation)
                    && s.commit_combat_transition(ship, token)
                    && s.remove_ball(ship).is_some()
                    && prepared.install_transfer(s, |s, ball| s.add_ship(ball), snapshot)
                    && s.combat_membership_generation(ship).is_some_and(|membership|
                        s.publish_loaded_absolute(ship, membership, settled_loaded_seeds(s, ship, &settled)))
            }).await.unwrap_or(false);
            if !installed { return false; }
        } else {
            let removed = world.with(self.source, move |s| {
                s.combat_session_current(character, expected.as_ref())
                    && s.combat_membership_generation(ship) == Some(generation)
                    && s.combat_phase(ship) == Some(crate::space::combat::CombatPhase::Transition { token })
                    && (!s.contains(ship) || (s.persistent_generation(ship) == Some(generation) && s.remove_ball(ship).is_some()))
            }).await.unwrap_or(false);
            if !removed { return false; }
            let scope = self.scope;
            let installed = world.with(self.dest, move |s| {
                if let Some(scope) = scope {
                    if let Err(error) = s.import_combat_character_scope(character, scope) {
                        tracing::warn!(?error, character, "combat qualification scope import refused");
                        return false;
                    }
                }
                prepared.install_transfer(s, |s, ball| s.add_ship(ball), snapshot)
                    && s.combat_membership_generation(ship).is_some_and(|membership|
                        s.publish_loaded_absolute(ship, membership, settled_loaded_seeds(s, ship, &settled)))
            }).await.unwrap_or(false);
            if !installed { return false; }
            world.with(self.source, move |s| s.commit_combat_transition(ship, token)).await;
        }
        crate::services::combat::reset_pilot(world, self.character, Some((self.source, self.ship)));
        let mut next = self.expected.clone();
        if let Some(session) = &mut next { session.system_id = Some(self.dest); }
        world.replace_online_session(self.character, self.expected.as_ref(), next);
        true
    }

    async fn reconcile(self: Arc<Self>) {
        let Some(world) = self.world.upgrade() else { return };
        if self.admission.0.check().is_err() || self.admission.1.as_ref().is_some_and(|admission| admission.check().is_err()) {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!(crate::space::containment::ActorRetiring))));
            return;
        }
        let _market = crate::services::market::lock().await;
        if self.admission.0.check().is_err() || self.admission.1.as_ref().is_some_and(|admission| admission.check().is_err()) {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!(crate::space::containment::ActorRetiring))));
            return;
        }
        if world.system_locked(self.dest, &_market).await.is_err() {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
            return;
        }
        let first = self.source.min(self.dest);
        let second = self.source.max(self.dest);
        let Ok(_first) = world.lock_condition_snapshot(first).await else {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
            return;
        };
        let _second = if first == second { None } else {
            match world.lock_condition_snapshot(second).await {
                Ok(gate) => Some(gate),
                Err(_) => {
                    let (ship, token, generation) = (self.ship, self.token, self.generation);
                    world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
                    return;
                }
            }
        };
        if !pending_loaded_retry(&world, &self.db, self.source, self.ship, self.token, self.generation).await { return; }
        let (receipt, verified) = match fact_receipt(&self.db, self.character, self.ship, &self.facts).await {
            Ok(receipt) => receipt,
            Err(_) => {
                let (ship, token, generation) = (self.ship, self.token, self.generation);
                world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
                return;
            }
        };
        // The bounty receipt proves itself under its own row-lock fence; a
        // publish or rollback-cancel needs the matching bounty proof.
        let bounty_outcome = match &self.bounties {
            Some(receipt) => match crate::services::bounty::npc::resolve_receipt(&self.db, receipt).await {
                Ok(outcome) => {
                    let _ = receipt.settle(&world, outcome).await;
                    Some(outcome)
                }
                Err(_) => None,
            },
            None => None,
        };
        use crate::services::bounty::npc::checkpoint::CheckpointOutcome::*;
        let bounty_durable = self.bounties.is_none() || bounty_outcome == Some(Durable);
        let bounty_unchanged = self.bounties.is_none() || bounty_outcome == Some(Unchanged);
        let bounty_consistent = self.bounties.is_none() || match (receipt, bounty_outcome) {
            (Some(true), Some(Durable)) => true,
            (Some(false), Some(Unchanged)) => true,
            _ => false,
        };
        let selected = crate::dao::characters::selection::space_ship(&self.db, self.character).await;
        let selected_ok = selected.is_ok();
        let current = match selected {
            Ok(Some(row)) if row.item_id == self.ship => row.x.zip(row.y).zip(row.z).map(|((x, y), z)| [x, y, z]),
            _ => None,
        };
        let prior_position = *crate::poison::lock_or_exit(self.prior_position.lock());
        if self.source == self.dest && prior_position == Some(self.position) && receipt.is_some()
            && bounty_consistent
            && (self.cloak_until.is_some() || receipt == Some(false)) {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
            return;
        }
        let dest_current = crate::dao::combat::victim_current(&self.db, self.character, self.ship, self.dest).await;
        let source_current = crate::dao::combat::victim_current(&self.db, self.character, self.ship, self.source).await;
        let mut at_dest = receipt != Some(false) && dest_current.as_ref().is_ok_and(|value| *value) && current == Some(self.position);
        if at_dest && self.source == self.dest && prior_position == Some(self.position)
            && self.snapshot.revision != self.snapshot.saved_revision {
            let rows = match crate::dao::items::dogma::overrides(&self.db, self.ship).await {
                Ok(rows) => rows,
                Err(_) => {
                    let (ship, token, generation) = (self.ship, self.token, self.generation);
                    world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
                    return;
                }
            };
            let condition = &self.snapshot.condition;
            let values = [condition.now[0], condition.stats.max[1] - condition.now[1], condition.stats.max[2] - condition.now[2]];
            let ids = [infinity_core::consts::dogma::ATTR_SHIELD_CHARGE, infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE, infinity_core::consts::dogma::ATTR_HULL_DAMAGE];
            at_dest = ids.iter().enumerate().all(|(i, id)| rows.iter().any(|row|
                row.attribute_id == *id && row.value == values[i] && (i != 0 || row.updated_at == condition.shield_at)));
        }
        if at_dest && receipt == Some(true) {
            acknowledge_facts(&world, self.source, self.facts.clone(), verified).await;
        }
        if at_dest && bounty_durable {
            if retire_stale_transition(&world, self.source, self.character, self.ship, self.token, self.generation, self.expected.clone()).await {
                self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship transfer session changed after commit"))));
                return;
            }
            let row = FreeShip {
                item_id: self.ship, type_id: self.original.type_id, owner_id: self.original.owner_id,
                corporation_id: self.original.corp_id, alliance_id: self.original.alliance_id, pos: self.position,
            };
            if let Ok(security) = crimewatch::security_status(&self.db, self.character).await {
                if let Ok(mut prepared) = balls::piloted(&self.db, world.sde().clone(), &row, self.character, security).await {
                    if let Some(until) = self.cloak_until {
                        prepared.ball.cloak = CLOAK_RESTORE;
                        prepared.ball.cloak_until = until;
                    }
                    if self.publish(&world, prepared, self.snapshot.clone(), Vec::new()).await {
                        self.complete_receipt(Ok(()));
                        return;
                    }
                }
            }
        } else if receipt != Some(true) && bounty_unchanged && current.is_some() && current == prior_position && source_current.as_ref().is_ok_and(|value| *value) {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            if world.with(self.source, move |s| s.cancel_combat_transition(ship, token, generation)).await.unwrap_or(false) {
                self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship transfer did not commit"))));
                return;
            }
        }
        if !at_dest && !(self.source == self.dest && current == prior_position) && selected_ok && dest_current.is_ok() && source_current.is_ok()
            && retire_authoritative_departure(&world, self.source, self.ship, self.token, self.generation).await {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship moved during transfer reconciliation"))));
            return;
        }
        let (ship, token, generation) = (self.ship, self.token, self.generation);
        world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
    }
}


impl System {
    /// Sample and authorize without changing actor state.
    pub(crate) fn authorize_board(&self, character: i64, old: i64, target: i64, max_gap: f64) -> Result<BoardSnapshot, BoardRefusal> {
        let old_ball = self.pilot_ball(character).filter(|b| b.id == old && b.char_id == Some(character) && b.owner_id == character)
            .ok_or(BoardRefusal::StalePilot)?;
        let old_generation = self.persistent_generation(old).ok_or(BoardRefusal::StalePilot)?;
        let is_ship = |b: &Ball| self.sde().and_then(|sde| sde.types.get(b.type_id))
            .is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP));
        if !is_ship(old_ball) { return Err(BoardRefusal::StalePilot); }
        if old == target { return Err(BoardRefusal::InvalidTarget); }
        let target_ball = self.ball(target).filter(|b| b.owner_id == character && is_ship(b))
            .ok_or(BoardRefusal::InvalidTarget)?;
        let target_generation = self.persistent_generation(target).ok_or(BoardRefusal::InvalidTarget)?;
        if target_ball.char_id.is_some() || target_ball.interactive { return Err(BoardRefusal::Occupied); }
        if old_ball.mode == MODE_WARP || target_ball.mode == MODE_WARP || old_ball.cloak != 0 || target_ball.cloak != 0 {
            return Err(BoardRefusal::Unsafe);
        }
        let old = self.sampled_ball(old).ok_or(BoardRefusal::StalePilot)?;
        let target = self.sampled_ball(target).ok_or(BoardRefusal::InvalidTarget)?;
        if !max_gap.is_finite() || max_gap < 0.0 || !old.radius.is_finite() || old.radius < 0.0
            || !target.radius.is_finite() || target.radius < 0.0
            || !old.pos.iter().chain(target.pos.iter()).chain(old.vel.iter()).chain(target.vel.iter()).all(|v| v.is_finite()) {
            return Err(BoardRefusal::Unsafe);
        }
        let distance = dist(old.pos, target.pos);
        if !distance.is_finite() || (distance - f64::from(old.radius) - f64::from(target.radius)).max(0.0) > max_gap {
            return Err(BoardRefusal::OutOfRange);
        }
        Ok(BoardSnapshot { old, target, old_generation, target_generation })
    }

}

#[cfg(all(test, feature = "tests-space"))]
#[path = "boarding/tests.rs"]
mod tests;
