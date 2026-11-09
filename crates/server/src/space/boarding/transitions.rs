//! Fenced publication and settlement of committed ship transitions.
//! Client ship boarding and stored-vessel operations use actor-owned condition
//! snapshots and game item rows; no independent condition state is maintained.
//! Restored old hull identity uses persistent membership, type and owner, not mutable motion.

use super::*;
use crate::space::combat::{ActorConditionSnapshot, CombatPhase};
use infinity_core::session::OnlineSession;

pub(crate) struct PreparedTransition {
    pub old: Option<PreparedOld>,
    pub target: PreparedTarget,
}

pub(crate) enum PreparedOld {
    Retained { snapshot_ball: Ball, prepared: PreparedShip },
    Stored { id: i64 },
}

pub(crate) struct PreparedTarget {
    pub id: i64,
    pub sampled_position: V3,
    pub prepared: PreparedShip,
    pub pilot: Option<(i64, i64)>,
    pub existing: bool,
}

use crate::dao::items::boarding::{Recovery, RecoveryPlan};

fn same_actor(s: &System, gate: &Arc<tokio::sync::Mutex<()>>) -> bool {
    s.snapshot_context.as_ref().is_some_and(|context| Arc::ptr_eq(&context.gate, gate))
}

pub(crate) async fn checkpoint<E: Send>(ex: &(impl crate::db::executor::Executor + ?Sized),
    outbox: &mut crate::db::outbox::Outbox<E>, system: i64, states: &[ActorConditionSnapshot],
    facts: Option<&crate::space::snapshot::FactCheckpoint>) -> Result<Vec<i64>, CallError> {
    let written = crate::services::combat::checkpoint_conditions_in(ex, outbox, system, states, facts).await?;
    if facts.is_some_and(|frame| frame.condition_rows().any(|row| !written.contains(&row.0))) {
        return Err(CallError::Internal(anyhow::anyhow!("collateral condition was not checkpointed")));
    }
    if states.iter().any(|state| state.revision != state.saved_revision && !written.contains(&state.item_id)) {
        return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
    }
    Ok(written)
}

impl PreparedTransition {
    fn committed(self, entries: &[(u64, ActorConditionSnapshot)], character: i64,
        expected: Option<OnlineSession>, written: &[i64], actor_gate: Arc<tokio::sync::Mutex<()>>) -> anyhow::Result<CommittedTransition> {
        let capture = |id| {
            entries.iter().find(|(_, state)| state.item_id == id).map(|(token, state)| {
                let mut state = state.clone();
                if written.contains(&id) { state.saved_revision = state.revision; }
                (*token, state)
            }).ok_or_else(|| anyhow::anyhow!("missing ship condition transition"))
        };
        let old = match self.old {
            Some(PreparedOld::Retained { snapshot_ball, prepared }) => {
                let (token, state) = capture(snapshot_ball.id)?;
                Some(CommittedOld::Retained { snapshot_ball, prepared, state, token })
            }
            Some(PreparedOld::Stored { id }) => {
                let (token, state) = capture(id)?;
                Some(CommittedOld::Stored { id, generation: state.generation, token })
            }
            None => None,
        };
        let existing = if self.target.existing {
            let (token, state) = capture(self.target.id)?;
            Some((state, token))
        } else { None };
        Ok(CommittedTransition { old, target: CommittedTarget {
            id: self.target.id, sampled_position: self.target.sampled_position,
            prepared: self.target.prepared, pilot: self.target.pilot, existing,
        }, character, expected, fresh: false, actor_gate: Some(actor_gate) })
    }
}

pub(crate) struct CommittedTransition {
    pub old: Option<CommittedOld>,
    pub target: CommittedTarget,
    pub character: i64,
    pub expected: Option<OnlineSession>,
    pub fresh: bool,
    pub actor_gate: Option<Arc<tokio::sync::Mutex<()>>>,
}

pub(crate) enum CommittedOld {
    Retained { snapshot_ball: Ball, prepared: PreparedShip, state: ActorConditionSnapshot, token: u64 },
    Stored { id: i64, generation: u64, token: u64 },
    Restored { id: i64, type_id: i32 },
    StoredFresh { id: i64 },
}

pub(crate) struct CommittedTarget {
    pub id: i64,
    pub sampled_position: V3,
    pub prepared: PreparedShip,
    pub pilot: Option<(i64, i64)>,
    pub existing: Option<(ActorConditionSnapshot, u64)>,
}

impl System {
    /// Publish only the reserved actor memberships and current session.
    pub(crate) fn apply_committed_transition(&mut self, transition: CommittedTransition) -> bool {
        let CommittedTransition { old, mut target, character, expected, fresh, actor_gate } = transition;
        let old_fence = old.as_ref().and_then(|old| match old {
            CommittedOld::Retained { snapshot_ball, state, token, .. } => Some((snapshot_ball.id, state.generation, *token)),
            CommittedOld::Stored { id, generation, token } => Some((*id, *generation, *token)),
            CommittedOld::Restored { .. } | CommittedOld::StoredFresh { .. } => None,
        });
        let target_fence = target.existing.as_ref().map(|(state, token)| (target.id, state.generation, *token));
        let valid_fence = |s: &System, (id, generation, token)| {
            s.persistent_generation(id) == Some(generation)
                && s.combat_membership_generation(id) == Some(generation)
                && s.combat_phase(id) == Some(CombatPhase::Transition { token })
        };
        if !self.combat_session_current(character, expected.as_ref()) { return false; }
        if actor_gate.as_ref().is_some_and(|gate| self.snapshot_context.as_ref()
            .is_none_or(|context| Arc::ptr_eq(&context.gate, gate) == fresh)) { return false; }
        if fresh {
            if self.pilot_ball(character).is_some() || self.contains(target.id) || target.existing.is_some() {
                return false;
            }
            match old.as_ref() {
                Some(CommittedOld::Restored { id, type_id }) if self.persistent_generation(*id).is_some()
                    && self.ball(*id).is_some_and(|ball| ball.char_id.is_none() && !ball.interactive
                        && ball.owner_id == character && ball.type_id == *type_id) => {}
                Some(CommittedOld::StoredFresh { id }) if !self.contains(*id) => {}
                _ => return false,
            }
        }
        if old_fence.is_some_and(|f| !valid_fence(self, f))
            || target_fence.is_some_and(|f| !valid_fence(self, f))
            || (target_fence.is_none() && self.contains(target.id))
            || old_fence.is_some_and(|(id, _, _)| self.pilot_ball(character).is_none_or(|b| b.id != id))
            || target.prepared.ball.id != target.id
            || target.prepared.ball.char_id != target.pilot.map(|(_, character)| character)
            || target.existing.as_ref().is_some_and(|(state, _)| !target.prepared.valid_transfer(state))
            || matches!(&old, Some(CommittedOld::Retained { prepared, state, .. }) if !prepared.valid_transfer(state)) {
            return false;
        }
        for id in old_fence.map(|f| f.0).into_iter().chain(target_fence.map(|f| f.0)) {
            if let Some(ball) = self.take(id) {
                if ball.cloak == 0 {
                    let update = event(self.current_time, "RemoveBalls", vec![Value::list([Value::Int(id)])]);
                    self.actions.push_back((self.current_time, Box::new(move |s, _| s.deliver(None, vec![update]))));
                }
            }
        }
        if let Some((client, _)) = target.pilot {
            if let Some((ego, _)) = self.observers.get_mut(&client) { *ego = target.id; }
        }
        if let Some(old) = old {
            match old {
                CommittedOld::Stored { id, token, .. } => { self.commit_combat_transition(id, token); }
                CommittedOld::Restored { .. } | CommittedOld::StoredFresh { .. } => {}
                CommittedOld::Retained { snapshot_ball, mut prepared, state, .. } => {
                    prepared.ball.pos = snapshot_ball.pos;
                    prepared.ball.stop();
                    prepared.ball.vel = [0.0; 3];
                    prepared.ball.speed_fraction = 0.0;
                    if !prepared.install_transfer(self, |s, ball| s.add_persistent_ship(ball), state) { return false; }
                }
            }
        }
        target.prepared.ball.pos = target.sampled_position;
        target.prepared.ball.stop();
        target.prepared.ball.vel = [0.0; 3];
        target.prepared.ball.speed_fraction = 0.0;
        if let Some((state, _)) = target.existing {
            target.prepared.install_transfer(self, |s, ball| s.add_ship(ball), state)
        } else {
            if target.pilot.is_some() { target.prepared.install(self, |s, ball| s.add_ship(ball)); }
            else { target.prepared.install(self, |s, ball| s.add_persistent_ship(ball)); }
            true
        }
    }
}

impl World {
    pub(crate) async fn apply_committed_transition(&self, system: i64, transition: CommittedTransition) -> anyhow::Result<()> {
        match self.with(system, move |s| s.apply_committed_transition(transition)).await {
            Some(true) => Ok(()),
            _ => Err(anyhow::anyhow!("committed ship transition membership changed or actor stopped")),
        }
    }
}

struct BoardSettlement {
    world: std::sync::Weak<World>,
    db: Db,
    sde: Arc<crate::sde::Sde>,
    system: i64,
    character: i64,
    client: i64,
    expected: OnlineSession,
    plan: RecoveryPlan,
    entries: Vec<(u64, ActorConditionSnapshot)>,
    facts: Option<Arc<crate::space::snapshot::FactCheckpoint>>,
    actor_gate: Arc<tokio::sync::Mutex<()>>,
    receipt: Mutex<Option<(tokio::sync::oneshot::Sender<Result<Option<i64>, CallError>>, Option<CallError>)>>,
}

impl BoardSettlement {
    fn finish(&self, result: Result<Option<i64>, &'static str>) -> bool {
        if let Some((sender, original)) = self.receipt.lock().unwrap().take() {
            let result = result.map_err(|message| original.unwrap_or_else(|| CallError::Internal(anyhow::anyhow!(message))));
            sender.send(result).is_ok()
        } else { false }
    }


    fn callback(self: &Arc<Self>) -> Arc<dyn Fn() -> crate::space::NpcFuture + Send + Sync> {
        let plan = self.clone();
        Arc::new(move || {
            let plan = plan.clone();
            Box::pin(async move { plan.reconcile().await })
        })
    }

    async fn mark_pending(self: &Arc<Self>, world: &World) -> bool {
        let entries: Vec<_> = self.entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
        let callback = self.callback();
        let gate = self.actor_gate.clone();
        world.with(self.system, move |s| {
            if !same_actor(s, &gate) { return false; }
            if !entries.iter().all(|&(id, token, generation)| s.combat_membership_generation(id) == Some(generation)
                && s.combat_phase(id) == Some(CombatPhase::Transition { token })) { return false; }
            for (id, token, generation) in entries { s.mark_pending_transition(id, token, generation, callback.clone()); }
            true
        }).await.unwrap_or(false)
    }

    async fn release(&self, world: &World) {
        let entries: Vec<_> = self.entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
        let gate = self.actor_gate.clone();
        let _ = world.with(self.system, move |s| {
            if !same_actor(s, &gate) { return; }
            for (id, token, generation) in entries { s.release_pending_combat(id, token, generation); }
        }).await;
    }

    async fn reconcile(self: Arc<Self>) {
        let Some(world) = self.world.upgrade() else { self.finish(Err("ship transition world stopped")); return };
        if !self.reconcile_once(&world).await { self.release(&world).await; }
    }

    async fn retire_stale(&self, world: &World, old: i64, old_row: Option<&crate::dao::items::space_ships::FreeShip>) -> bool {
        let stored = matches!(self.plan, RecoveryPlan::Store { .. });
        let prepared = if stored { None } else {
            let Some(row) = old_row else { return false };
            let Ok(ship) = balls::unpiloted(&self.db, self.sde.clone(), row).await else { return false };
            Some(ship)
        };
        let old_facts = old_row.map(|row| (row.type_id, row.owner_id));
        let entries: Vec<_> = self.entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
        let gate = self.actor_gate.clone();
        world.with(self.system, move |s| {
            let entries = if same_actor(s, &gate) { entries } else { Vec::new() };
            for &(id, token, generation) in &entries {
                if s.combat_membership_generation(id) == Some(generation)
                    && s.combat_phase(id) != Some(CombatPhase::Transition { token }) { return false; }
                if s.combat_membership_generation(id) == Some(generation)
                    && s.contains(id) && s.persistent_generation(id) != Some(generation) { return false; }
            }
            let old_reserved = entries.iter().any(|&(id, token, generation)| id == old
                && s.combat_membership_generation(id) == Some(generation)
                && s.combat_phase(id) == Some(CombatPhase::Transition { token }));
            if !old_reserved && s.contains(old) && (stored || s.persistent_generation(old).is_none()
                || s.ball(old).is_none_or(|ball| ball.char_id.is_some() || ball.interactive
                    || old_facts != Some((ball.type_id, ball.owner_id)))) {
                return false;
            }
            for (id, token, generation) in entries {
                if s.combat_membership_generation(id) == Some(generation)
                    && s.combat_phase(id) == Some(CombatPhase::Transition { token }) {
                    if s.contains(id) { s.remove_ball(id); } else { s.commit_combat_transition(id, token); }
                }
            }
            if !stored && !s.contains(old) {
                if let Some(prepared) = prepared { prepared.install(s, |s, ball| s.add_persistent_ship(ball)); }
            }
            true
        }).await.unwrap_or(false)
    }

    async fn reconcile_once(&self, world: &World) -> bool {
        use crate::dao::items::boarding;
        let market = crate::services::market::lock().await;
        if world.system_locked(self.system, &market).await.is_err() { return false; }
        let Ok(_gate) = world.lock_condition_snapshot(self.system).await else { return false };
        let old = match self.plan {
            RecoveryPlan::Board { old, .. } | RecoveryPlan::Store { old, .. } | RecoveryPlan::BoardStored { old, .. } => old,
        };
        let roots = match self.plan {
            RecoveryPlan::Board { old, target, .. } => vec![old, target],
            RecoveryPlan::Store { old, host, .. } => vec![old, host],
            RecoveryPlan::BoardStored { old, host, target, .. } => vec![old, host, target],
        };
        let character = self.character;
        let plan = self.plan;
        let facts = self.facts.clone();
        let sde = self.sde.clone();
        let read = self.db.transaction(|tx, _: &mut ()| {
            let roots = roots.clone();
            let facts = facts.clone();
            let sde = sde.clone();
            Box::pin(async move {
                use crate::dao::items::{deliveries, inventory, parent_locks::{LockRequest, LockStrength}};
                use crate::services::{db_err, inventory::destination::{lock_items, parent_requests}};
                crate::dao::characters::selection::lock_character(tx, character).await.map_err(db_err)?;
                let selected = if matches!(plan, RecoveryPlan::Store { .. }) {
                    crate::dao::characters::selection::ship_id(tx, character).await.map_err(db_err)?
                } else { None };
                let lineages = deliveries::ancestry_many(tx, &roots).await.map_err(db_err)?;
                let closure = inventory::descendants(tx, &roots).await.map_err(db_err)?;
                let mut requests: Vec<_> = closure.iter().copied().chain(selected)
                    .map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
                requests.extend(parent_requests(&sde,
                    lineages.iter().map(|row| (row.ancestor.item_id, row.ancestor.type_id)),
                    LockStrength::Update, LockStrength::Share)?);
                let locks = lock_items(tx, &sde, &requests, &[]).await?;
                if inventory::descendants(tx, &roots).await.map_err(db_err)? != closure
                    || deliveries::ancestry_many(tx, &roots).await.map_err(db_err)? != lineages {
                    return Err(CallError::Internal(anyhow::anyhow!("stale ship transition recovery")));
                }
                let outcome = boarding::recovery(tx, &plan).await.map_err(db_err)?;
                if let Recovery::Committed { target, old } = &outcome {
                    if locks.row(target.item_id).is_none() || old.as_ref().is_some_and(|row| locks.row(row.item_id).is_none())
                        || (matches!(plan, RecoveryPlan::Store { .. }) && selected != Some(target.item_id)) {
                        return Err(CallError::Internal(anyhow::anyhow!("unlocked ship transition recovery")));
                    }
                }
                let (receipt, written) = match &facts {
                    Some(frame) => {
                        let committed = frame.is_committed(tx).await.map_err(db_err)?;
                        let written = if committed { frame.verified_condition_ids(tx).await.map_err(db_err)? } else { Default::default() };
                        (Some(committed), written)
                    }
                    None => (None, Default::default()),
                };
                Ok::<_, CallError>((outcome, receipt, written))
            })
        }, |value, ()| async move { Ok(value) }).await;
        let Ok((outcome, receipt, verified)) = read else { return false };
        if matches!(outcome, Recovery::Committed { .. }) && receipt == Some(false) { return false; }
        if matches!(outcome, Recovery::Committed { .. }) {
            if let Some(frame) = self.facts.clone() {
                let gate = self.actor_gate.clone();
                let written = verified.clone();
                let _ = world.with(self.system, move |s| {
                    if same_actor(s, &gate) { frame.acknowledge(s, &written); }
                }).await;
            }
        }
        if let Recovery::Committed { target: ref target_row, old: ref old_row } = outcome {
            let target_id = target_row.item_id;
            if world.online_session(self.character).as_ref() != Some(&self.expected) {
                if self.retire_stale(world, old, old_row.as_ref()).await {
                    self.finish(Err("ship transition session changed"));
                    return true;
                }
                return false;
            }
            let Ok(security) = crimewatch::security_status(&self.db, self.character).await else { return false };
            let entries: Vec<_> = self.entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
            let gate = self.actor_gate.clone();
            let reserved = world.with(self.system, move |s| same_actor(s, &gate) && entries.iter().all(|&(id, token, generation)|
                s.combat_membership_generation(id) == Some(generation)
                    && s.combat_phase(id) == Some(CombatPhase::Transition { token }))).await.unwrap_or(false);
            let Ok(target) = balls::piloted(&self.db, self.sde.clone(), target_row, self.character, security).await else { return false };
            let transition = if reserved {
                let old_prepared = if matches!(self.plan, RecoveryPlan::Store { .. }) {
                    Some(PreparedOld::Stored { id: old })
                } else {
                    let Some(row) = old_row.as_ref() else { return false };
                    let Ok(prepared) = balls::unpiloted(&self.db, self.sde.clone(), row).await else { return false };
                    Some(PreparedOld::Retained { snapshot_ball: prepared.ball.clone(), prepared })
                };
                let prepared = PreparedTransition { old: old_prepared, target: PreparedTarget {
                    id: target_id, sampled_position: target_row.pos, prepared: target,
                    pilot: Some((self.client, self.character)), existing: matches!(self.plan, RecoveryPlan::Board { .. }),
                }};
                let written: Vec<_> = if self.facts.is_some() {
                    verified.iter().copied().collect()
                } else { self.entries.iter().map(|(_, state)| state.item_id).collect() };
                let Ok(transition) = prepared.committed(&self.entries, self.character, Some(self.expected.clone()), &written, self.actor_gate.clone()) else { return false };
                transition
            } else {
                let replacement = world.systems.lock().unwrap().get(&self.system)
                    .is_some_and(|handle| !Arc::ptr_eq(&handle.gate, &self.actor_gate));
                if !replacement { return false; }
                let receiver_live = self.receipt.lock().unwrap().as_ref().is_some_and(|(sender, _)| !sender.is_closed());
                if !receiver_live { self.finish(Err("ship transition caller disconnected")); return true; }
                CommittedTransition {
                    old: Some(if matches!(self.plan, RecoveryPlan::Store { .. }) {
                        CommittedOld::StoredFresh { id: old }
                    } else {
                        let Some(row) = old_row.as_ref() else { return false };
                        CommittedOld::Restored { id: old, type_id: row.type_id }
                    }),
                    target: CommittedTarget { id: target_id, sampled_position: target_row.pos, prepared: target,
                        pilot: Some((self.client, self.character)), existing: None },
                    character: self.character, expected: Some(self.expected.clone()), fresh: true, actor_gate: Some(self.actor_gate.clone()),
                }
            };
            if world.apply_committed_transition(self.system, transition).await.is_ok() {
                self.finish(Ok(Some(target_id)));
                return true;
            }
        } else if matches!(outcome, Recovery::Aborted) {
            {
                let entries: Vec<_> = self.entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
                let gate = self.actor_gate.clone();
                let restored = world.with(self.system, move |s| {
                    if !same_actor(s, &gate) || !entries.iter().all(|&(id, token, generation)| s.combat_membership_generation(id) == Some(generation)
                        && s.combat_phase(id) == Some(CombatPhase::Transition { token })) { return false; }
                    for (id, token, generation) in entries { s.cancel_combat_transition(id, token, generation); }
                    true
                }).await.unwrap_or(false);
                if restored { self.finish(Err("ship transition did not commit")); return true; }
            }
        }
        false
    }
}

pub(crate) async fn settle_space<F>(
    world: Arc<World>, db: Db, sde: Arc<crate::sde::Sde>, system: i64, character: i64, client: i64,
    expected: OnlineSession, transition: CombatTransition,
    market: tokio::sync::MutexGuard<'static, ()>, gate: tokio::sync::OwnedMutexGuard<()>,
    plan: RecoveryPlan, operation: F,
) -> Result<Option<i64>, CallError>
where
    F: for<'a, 'tx> Fn(&'a tokio_postgres::Transaction<'tx>,
        &'a mut crate::db::outbox::Outbox<crate::services::effects::online::OnlineChanged>,
        &'a [ActorConditionSnapshot], Option<&'a crate::space::snapshot::FactCheckpoint>) -> crate::db::transaction::TxFuture<'a, (PreparedTransition, Vec<i64>), CallError>
        + Send + Sync + 'static,
{
    use crate::services::effects::online;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let actor_gate = world.systems.lock().unwrap().get(&system).map(|handle| handle.gate.clone())
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("ship transition actor stopped")))?;
    let settlement = Arc::new(BoardSettlement {
        world: Arc::downgrade(&world), db: db.clone(), sde, system, character, client,
        expected, plan, entries: transition.entries.clone(), facts: transition.facts.clone(), actor_gate, receipt: Mutex::new(Some((sender, None))),
    });
    let submit_world = world.clone();
    let submitted = world.with(system, move |s| s.defer_condition_transition(transition, move |_| async move {
        let states: Vec<_> = settlement.entries.iter().map(|(_, state)| state.clone()).collect();
        if submit_world.online_session(character).as_ref() != Some(&settlement.expected) {
            let entries: Vec<_> = settlement.entries.iter().map(|(token, state)| (state.item_id, *token, state.generation)).collect();
            let actor_gate = settlement.actor_gate.clone();
            let _ = submit_world.with(system, move |s| {
                if same_actor(s, &actor_gate) && entries.iter().all(|&(id, token, generation)| s.combat_membership_generation(id) == Some(generation)
                    && s.combat_phase(id) == Some(CombatPhase::Transition { token })) {
                    for (id, token, generation) in entries { s.cancel_combat_transition(id, token, generation); }
                }
            }).await;
            settlement.finish(Err("ship transition session changed before transaction"));
            return;
        }
        let operation = Arc::new(operation);
        let result: Result<i64, CallError> = db.transaction(
            |tx, outbox: &mut crate::db::outbox::Outbox<online::OnlineChanged>| {
                let states = states.clone();
                let operation = operation.clone();
                let facts = settlement.facts.clone();
                Box::pin(async move {
                    let (prepared, written) = operation(tx, outbox, &states, facts.as_deref()).await?;
                    if states.iter().any(|state| state.revision != state.saved_revision && !written.contains(&state.item_id)) {
                        return Err(CallError::Internal(anyhow::anyhow!("ship condition was not checkpointed")));
                    }
                    Ok((prepared, written))
                })
            },
            |(prepared, written), outbox| {
                let world = &submit_world;
                let settlement = &settlement;
                async move {
                    let target = prepared.target.id;
                    let transition = prepared.committed(&settlement.entries, character, Some(settlement.expected.clone()), &written, settlement.actor_gate.clone())
                        .map_err(CallError::Internal)?;
                    if let Some(frame) = settlement.facts.clone() {
                        let gate = settlement.actor_gate.clone();
                        let acknowledged = written.iter().copied().collect();
                        let _ = world.with(system, move |s| {
                            if same_actor(s, &gate) { frame.acknowledge(s, &acknowledged); }
                        }).await;
                    }
                    online::dispatch_changed_world((), outbox, world)?;
                    world.apply_committed_transition(system, transition).await.map_err(CallError::Internal)?;
                    Ok(target)
                }
            },
        ).await;
        match result {
            Ok(target) => {
                settlement.finish(Ok(Some(target)));
                return;
            }
            Err(error) => {
                if let Some((_, original)) = settlement.receipt.lock().unwrap().as_mut() { *original = Some(error); }
            }
        }
        let pending = settlement.mark_pending(&submit_world).await;
        drop(gate);
        drop(market);
        if pending { settlement.reconcile().await; }
        else if !settlement.reconcile_once(&submit_world).await {
            settlement.finish(Err("ship transition actor stopped during reconciliation"));
        }
    })).await.unwrap_or(false);
    if !submitted { return Err(CallError::Internal(anyhow::anyhow!("ship transition worker unavailable"))); }
    receiver.await.map_err(|_| CallError::Internal(anyhow::anyhow!("ship transition worker unavailable")))?
}

#[cfg(test)]
#[path = "transitions/tests.rs"]
mod tests;
