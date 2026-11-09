//! Ship motion authorization and condition-checkpoint settlements for boarding,
//! docking and transfer. State comes from game items and the immutable SDE.
//! Actor transition tokens fence concurrent combat and session replacement.
use super::*;
use std::future::Future;
use infinity_core::consts::CATEGORY_SHIP;
use crate::dao::items::space_ships::FreeShip;
use crate::services::{crimewatch, ship::balls::{self, PreparedShip}, CallError};

#[derive(Clone)]
pub(crate) struct BoardSnapshot {
    pub old: Ball,
    pub target: Ball,
    pub old_generation: u64,
    pub target_generation: u64,
}

#[path = "boarding/transitions.rs"]
pub(crate) mod transitions;

#[derive(Debug, PartialEq)]
pub(crate) enum BoardRefusal { StalePilot, InvalidTarget, Occupied, Unsafe, OutOfRange }

/// Cancels reservations even when the requesting future is dropped.
pub(crate) struct CombatTransition {
    world: std::sync::Weak<World>,
    system: i64,
    pub(crate) entries: Vec<(u64, crate::space::combat::ActorConditionSnapshot)>,
    pub(crate) facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
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
        Some(CombatTransition { world: self.world.clone(), system: self.id, entries, facts, scope, armed: true })
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

struct DockSettlement {
    world: std::sync::Weak<World>,
    db: Db,
    character: i64,
    ship: i64,
    system: i64,
    station: i64,
    token: u64,
    generation: u64,
    facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
    expected: Option<infinity_core::session::OnlineSession>,
    receipt: Mutex<Option<tokio::sync::oneshot::Sender<Result<(), CallError>>>>,
}

impl DockSettlement {
    fn complete_receipt(&self, result: Result<(), CallError>) {
        if let Some(sender) = self.receipt.lock().unwrap().take() { let _ = sender.send(result); }
    }

    async fn publish(&self, world: &World) -> bool {
        let (ship, token, generation) = (self.ship, self.token, self.generation);
        let character = self.character;
        let expected = self.expected.clone();
        let removed = world.with(self.system, move |s| {
            (character == 0 || s.combat_session_current(character, expected.as_ref()))
                && s.combat_membership_generation(ship) == Some(generation)
                && s.persistent_generation(ship) == Some(generation)
                && s.commit_combat_transition(ship, token) && s.remove_ball(ship).is_some()
        }).await.unwrap_or(false);
        if !removed { return false; }
        if self.character != 0 {
            crate::services::combat::reset_pilot(world, self.character, Some((self.system, self.ship)));
            let mut next = self.expected.clone();
            if let Some(session) = &mut next {
                session.station_id = Some(self.station);
                session.system_id = Some(self.system);
            }
            world.replace_online_session(self.character, self.expected.as_ref(), next);
        }
        true
    }

    async fn reconcile(self: Arc<Self>) {
        let Some(world) = self.world.upgrade() else { return };
        let market = crate::services::market::lock().await;
        let Ok(gate) = world.lock_condition_snapshot(self.system).await else {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            world.with(self.system, move |s| s.release_pending_combat(ship, token, generation)).await;
            return;
        };
        let (receipt, verified) = match fact_receipt(&self.db, self.character, self.ship, &self.facts).await {
            Ok(receipt) => receipt,
            Err(_) => {
                let (ship, token, generation) = (self.ship, self.token, self.generation);
                world.with(self.system, move |s| s.release_pending_combat(ship, token, generation)).await;
                return;
            }
        };
        let item = crate::dao::items::loading::load_items(&self.db, crate::dao::items::loading::ItemQuery::Id(self.ship)).await;
        let selected = if self.character == 0 { Ok(Some(self.ship)) }
            else { crate::dao::characters::selection::ship_id(&self.db, self.character).await };
        let (location, authoritative, read_ok) = match (item, selected) {
            (Ok(items), Ok(selected)) => {
                let location = items.into_iter().next().map(|item| item.location_id);
                (location, selected == Some(self.ship), true)
            }
            _ => (None, false, false),
        };
        let changed = read_ok && (!authoritative || location.is_none_or(|id| id != self.system && id != self.station));
        if changed && retire_authoritative_departure(&world, self.system, self.ship, self.token, self.generation).await {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship moved during dock reconciliation"))));
            return;
        }
        let location = if authoritative { location } else { None };
        if location == Some(self.station) && receipt == Some(true) {
            acknowledge_facts(&world, self.system, self.facts.clone(), verified).await;
        }
        if location == Some(self.station) && self.character != 0
            && retire_stale_transition(&world, self.system, self.character, self.ship, self.token, self.generation, self.expected.clone()).await {
            self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("dock session changed after commit"))));
            return;
        }
        if receipt != Some(false) && location == Some(self.station) && self.publish(&world).await {
            drop(gate);
            drop(market);
            if self.character != 0 {
                if let Err(error) = crate::services::corps::pending_kicks::trigger(&self.db, &world, self.character).await {
                    tracing::warn!(?error, character = self.character, "completed dock pending kick failed");
                }
            }
            self.complete_receipt(Ok(()));
            return;
        }
        let unchanged = receipt != Some(true) && location == Some(self.system) && (self.character == 0
            || crate::dao::combat::victim_current(&self.db, self.character, self.ship, self.system).await.unwrap_or(false));
        if unchanged {
            let (ship, token, generation) = (self.ship, self.token, self.generation);
            let restored = world.with(self.system, move |s| s.cancel_combat_transition(ship, token, generation)).await.unwrap_or(false);
            if restored {
                self.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("dock transaction did not commit"))));
                return;
            }
        }
        let (ship, token, generation) = (self.ship, self.token, self.generation);
        world.with(self.system, move |s| s.release_pending_combat(ship, token, generation)).await;
    }

    fn callback(self: &Arc<Self>) -> Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> {
        let plan = self.clone();
        Arc::new(move || {
            let plan = plan.clone();
            Box::pin(async move { plan.reconcile().await })
        })
    }
}

impl World {
    pub(crate) async fn checkpoint_dock(&self, db: Db, character: i64, ship: i64, system: i64, station: i64,
        transition: CombatTransition, market: tokio::sync::MutexGuard<'static, ()>,
        gate: tokio::sync::OwnedMutexGuard<()>) -> Result<(), CallError> {
        let world = transition.world().ok_or_else(|| CallError::Internal(anyhow::anyhow!("system world stopped")))?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let plan = Arc::new(DockSettlement {
            world: Arc::downgrade(&world), db, character, ship, system, station,
            token: transition.entries[0].0, generation: transition.entries[0].1.generation,
            facts: transition.facts.clone(),
            expected: self.online_session(character), receipt: Mutex::new(Some(tx)),
        });
        let submitted = self.with(system, move |s| s.defer_condition_transition(transition, move |transition| async move {
            let snapshot = transition.entries[0].1.clone();
            let result: Result<bool, CallError> = plan.db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let snapshot = snapshot.clone();
                    let facts = plan.facts.clone();
                    Box::pin(async move {
                        if character != 0 {
                            crate::dao::characters::selection::lock_character(tx, character).await.map_err(crate::services::db_err)?;
                        }
                        if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(crate::services::db_err)?
                            || (character != 0 && !crate::dao::combat::victim_current(tx, character, ship, system).await.map_err(crate::services::db_err)?) {
                            return Err(CallError::Internal(anyhow::anyhow!("ship is no longer available for docking")));
                        }
                        let written = crate::services::combat::checkpoint_conditions_in(tx, outbox, system, std::slice::from_ref(&snapshot), facts.as_deref()).await?;
                        if snapshot.revision != snapshot.saved_revision && !written.contains(&ship) {
                            return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
                        }
                        crate::dao::items::ship::dock(tx, ship, station, infinity_core::consts::FLAG_HANGAR).await.map_err(crate::services::db_err)?;
                        crate::dao::characters::ship::dock(tx, character, station, system).await.map_err(crate::services::db_err)?;
                        if character != 0 {
                            outbox.event(crate::lua::events::GameEvent::Docked { char_id: character, ship_id: ship, station_id: station, system_id: system });
                        }
                        Ok(written)
                    })
                }, |written, outbox| {
                    let plan = &plan;
                    let world = &world;
                    async move {
                    acknowledge_facts(world, system, plan.facts.clone(), written.into_iter().collect()).await;
                    let published = plan.publish(&world).await;
                    crate::db::outbox::dispatch_world(published, outbox, world)
                    }
                },
            ).await;
            if matches!(result, Ok(true)) {
                drop(gate);
                drop(market);
                if character != 0 {
                    if let Err(error) = crate::services::corps::pending_kicks::trigger(&plan.db, &world, character).await {
                        tracing::warn!(?error, character, "completed dock pending kick failed");
                    }
                }
                plan.complete_receipt(Ok(()));
                return;
            }
            let callback = plan.callback();
            let (token, generation) = (plan.token, plan.generation);
            let pending = world.with(system, move |s| s.mark_pending_transition(ship, token, generation, callback)).await.unwrap_or(false);
            drop(gate);
            drop(market);
            if pending { plan.reconcile().await; }
            else { plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("system actor stopped during dock settlement")))); }
        })).await.unwrap_or(false);
        if !submitted { return Err(CallError::Internal(anyhow::anyhow!("system worker unavailable"))); }
        rx.await.map_err(|_| CallError::Internal(anyhow::anyhow!("dock worker unavailable")))?
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
    scope: Option<crate::space::combat_facts::boundaries::Boundary>,
    expected: Option<infinity_core::session::OnlineSession>,
    cloak_until: Option<i64>,
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
        if let Some(sender) = self.receipt.lock().unwrap().take() { let _ = sender.send(result); }
    }

    async fn publish(&self, world: &World, prepared: PreparedShip, snapshot: crate::space::combat::ActorConditionSnapshot) -> bool {
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
        let _market = crate::services::market::lock().await;
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
        let (receipt, verified) = match fact_receipt(&self.db, self.character, self.ship, &self.facts).await {
            Ok(receipt) => receipt,
            Err(_) => {
                let (ship, token, generation) = (self.ship, self.token, self.generation);
                world.with(self.source, move |s| s.release_pending_combat(ship, token, generation)).await;
                return;
            }
        };
        let selected = crate::dao::characters::selection::space_ship(&self.db, self.character).await;
        let selected_ok = selected.is_ok();
        let current = match selected {
            Ok(Some(row)) if row.item_id == self.ship => row.x.zip(row.y).zip(row.z).map(|((x, y), z)| [x, y, z]),
            _ => None,
        };
        let prior_position = *self.prior_position.lock().unwrap();
        if self.source == self.dest && prior_position == Some(self.position) && receipt.is_some()
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
        if at_dest {
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
                    if self.publish(&world, prepared, self.snapshot.clone()).await {
                        self.complete_receipt(Ok(()));
                        return;
                    }
                }
            }
        } else if receipt != Some(true) && current.is_some() && current == prior_position && source_current.as_ref().is_ok_and(|value| *value) {
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

impl World {
    pub(crate) async fn checkpoint_transfer(&self, db: Db, character: i64, ship: i64, source: i64, dest: i64,
        position: V3, ancestry: (i64, i64), prepared: PreparedShip, cloak_until: Option<i64>,
        transition: CombatTransition, market: tokio::sync::MutexGuard<'static, ()>,
        gates: Vec<tokio::sync::OwnedMutexGuard<()>>) -> Result<(), CallError> {
        let world = transition.world().ok_or_else(|| CallError::Internal(anyhow::anyhow!("system world stopped")))?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let plan = Arc::new(TransferSettlement {
            world: Arc::downgrade(&world), db, character, ship, source, dest,
            original: prepared.ball.clone(), position, prior_position: Mutex::new(None),
            token: transition.entries[0].0, generation: transition.entries[0].1.generation,
            snapshot: transition.entries[0].1.clone(), expected: self.online_session(character),
            facts: transition.facts.clone(),
            scope: transition.scope.filter(|(owner, _)| *owner == character).map(|(_, scope)| scope),
            cloak_until, receipt: Mutex::new(Some(tx)),
        });
        let submitted = self.with(source, move |s| s.defer_condition_transition(transition, move |_| async move {
            let prior = crate::dao::characters::selection::space_ship(&plan.db, character).await;
            let prior = match prior {
                Ok(Some(row)) if row.item_id == ship => row.x.zip(row.y).zip(row.z).map(|((x, y), z)| [x, y, z]),
                _ => None,
            };
            if prior.is_none() {
                let (token, generation) = (plan.token, plan.generation);
                world.with(source, move |s| s.cancel_combat_transition(ship, token, generation)).await;
                plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("ship position unavailable before transfer"))));
                return;
            }
            *plan.prior_position.lock().unwrap() = prior;
            let occurrence = if cloak_until.is_some() {
                match crate::dao::map_telemetry::next_occurrence(&plan.db).await {
                    Ok(id) => Some((id, crate::client::filetime_now())),
                    Err(error) => {
                        let (token, generation) = (plan.token, plan.generation);
                        world.with(source, move |s| s.cancel_combat_transition(ship, token, generation)).await;
                        plan.complete_receipt(Err(crate::services::db_err(error)));
                        return;
                    }
                }
            } else { None };
            let result: Result<bool, CallError> = plan.db.transaction(
                |tx, outbox: &mut crate::db::outbox::Outbox| {
                    let snapshot = plan.snapshot.clone();
                    let facts = plan.facts.clone();
                    Box::pin(async move {
                        crate::dao::characters::selection::lock_character(tx, character).await.map_err(crate::services::db_err)?;
                        if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(crate::services::db_err)?
                            || !crate::dao::combat::victim_current(tx, character, ship, source).await.map_err(crate::services::db_err)? {
                            return Err(CallError::Internal(anyhow::anyhow!("ship is no longer available for transfer")));
                        }
                        let written = crate::services::combat::checkpoint_conditions_in(tx, outbox, source, std::slice::from_ref(&snapshot), facts.as_deref()).await?;
                        if snapshot.revision != snapshot.saved_revision && !written.contains(&ship) {
                            return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
                        }
                        crate::dao::items::ship::position(tx, ship, dest, position).await.map_err(crate::services::db_err)?;
                        if source != dest {
                            crate::dao::characters::ship::location(tx, character, dest, ancestry.0, ancestry.1).await.map_err(crate::services::db_err)?;
                        }
                        if let Some((occurrence, now)) = occurrence {
                            if crate::dao::map_telemetry::append(tx, crate::dao::map_telemetry::EventSource::JumpOccurrence, occurrence, dest, now, crate::dao::map_telemetry::EventKind::Jump).await.map_err(crate::services::db_err)? {
                                crate::dao::map_telemetry::record_visit(tx, character, dest, now).await.map_err(crate::services::db_err)?;
                            }
                            outbox.event_from(crate::db::outbox::SourceKey::new("Jumped", occurrence),
                                crate::lua::events::GameEvent::Jumped { char_id: character, ship_id: ship, from_system_id: source, to_system_id: dest });
                        }
                        Ok(written)
                    })
                }, |written, outbox| {
                    let plan = &plan;
                    let world = &world;
                    async move {
                    acknowledge_facts(world, source, plan.facts.clone(), written.iter().copied().collect()).await;
                    let mut snapshot = plan.snapshot.clone();
                    if written.contains(&ship) { snapshot.saved_revision = snapshot.revision; }
                    let published = plan.publish(&world, prepared, snapshot).await;
                    crate::db::outbox::dispatch_world(published, outbox, world)
                    }
                },
            ).await;
            if matches!(result, Ok(true)) {
                plan.complete_receipt(Ok(()));
                return;
            }
            let callback = plan.callback();
            let (token, generation) = (plan.token, plan.generation);
            let pending = world.with(source, move |s| s.mark_pending_transition(ship, token, generation, callback)).await.unwrap_or(false);
            drop(gates);
            drop(market);
            if pending { plan.reconcile().await; }
            else { plan.complete_receipt(Err(CallError::Internal(anyhow::anyhow!("system actor stopped during transfer settlement")))); }
        })).await.unwrap_or(false);
        if !submitted { return Err(CallError::Internal(anyhow::anyhow!("system worker unavailable"))); }
        rx.await.map_err(|_| CallError::Internal(anyhow::anyhow!("ship transfer worker unavailable")))?
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

#[cfg(test)]
#[path = "boarding/tests.rs"]
mod tests;
