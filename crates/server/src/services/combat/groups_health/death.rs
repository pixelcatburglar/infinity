//! Durable NPC death journal, fenced by the actor's existing dying transition.
//! Temporary NPCs have no persistent domain row; their journal source is the tombstone.
//! Bounty and wreck processing remain separate from the authoritative death transaction.
use super::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::db::outbox::SourceKey;
use crate::lua::events::{GameEvent, combat::BinaryAmount};
use crate::space::{combat::FirstLethalSnapshot, snapshot::FactCheckpoint};

pub(crate) struct NpcDeathContext {
    frame: Option<Arc<FactCheckpoint>>,
    event: GameEvent,
    ball: Ball,
    had_unknown_commit: AtomicBool,
}

struct DeathEffect { key: String, fresh: bool }
impl crate::progress::Prepare for DeathEffect {
    fn prepare<'a, 'tx>(&'a mut self, _: &'a tokio_postgres::Transaction<'tx>, _: &'a mut crate::progress::PrepareCx) -> crate::db::transaction::TxFuture<'a, (), crate::progress::PrepareError> {
        Box::pin(async { Ok(()) })
    }
}
impl crate::progress::AfterCommit for DeathEffect {
    fn after_commit(&mut self, registry: &crate::progress::Registry, committed: &std::collections::HashSet<String>) -> crate::progress::AfterCommitHandle {
        self.fresh = committed.contains(&self.key);
        crate::progress::AfterCommitHandle::empty(registry)
    }
}

fn death_key(snapshot: &FirstLethalSnapshot) -> SourceKey {
    SourceKey::canonical("NpcDestroyed", format!("{:032x}/{}/{}", u128::from_be_bytes(snapshot.victim.incarnation), snapshot.item_id, snapshot.token), 0)
}


fn death_event(snapshot: &FirstLethalSnapshot, system: i64, bounty: f64) -> Result<GameEvent, CallError> {
    let killer = snapshot.source.facts.and_then(|source| source.controller_entity());
    let killer_id = killer.and_then(|entity| entity.character_id);
    Ok(GameEvent::NpcDestroyed { death_id: death_key(snapshot).as_str().to_owned(),
        spawn_generation: snapshot.generation, npc: snapshot.victim, system_id: system,
        killer_id, killer, occurred_at: snapshot.lethal_at,
        base_bounty: BinaryAmount::from_f64(bounty).map_err(CallError::Internal)?,
        static_version: None, context: snapshot.source.facts.map_or(snapshot.victim_context, |source| source.context) })
}

async fn release(world: &World, system: i64, snapshot: &FirstLethalSnapshot) {
    let (item, token, generation) = (snapshot.item_id, snapshot.token, snapshot.generation);
    world.with(system, move |s| { s.release_pending_combat(item, token, generation); }).await;
}

async fn finalize(world: &World, system: i64, snapshot: &FirstLethalSnapshot, frame: Option<Arc<FactCheckpoint>>, written: std::collections::HashSet<i64>) -> Result<bool, CallError> {
    let (item, token, generation) = (snapshot.item_id, snapshot.token, snapshot.generation);
    let removed = world.with(system, move |s| {
        if let Some(frame) = frame { frame.acknowledge(s, &written); }
        if s.combat_phase(item) != Some(crate::space::combat::CombatPhase::Dying { token })
            || s.combat_membership_generation(item) != Some(generation) { return false; }
        s.destroy_ball(item);
        s.finalize_dying(item, token, generation);
        true
    }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("NPC death actor unavailable")))?;
    if removed { forget_target(world, item); }
    Ok(removed)
}

pub(crate) async fn settle_npc_loss(db: Db, world: Arc<World>, system: i64, snapshot: Arc<FirstLethalSnapshot>, _new: [f64; 3]) -> Result<(), CallError> {
    let gate = world.lock_condition_snapshot(system).await.map_err(db_err);
    let _gate = match gate { Ok(gate) => gate, Err(error) => { release(&world, system, &snapshot).await; return Err(error); } };
    let item = snapshot.item_id;
    let token = snapshot.token;
    let generation = snapshot.generation;
    let existing = world.with(system, move |s| s.npc_death_context(item, token, generation)).await;
    let context = match existing {
        Some(Some(context)) => Some(context),
        None => { release(&world, system, &snapshot).await; return Err(CallError::Internal(anyhow::anyhow!("NPC death actor unavailable"))); }
        Some(None) => {
            let bounty = attr(world.sde(), snapshot.victim.type_id, ATTR_ENTITY_KILL_BOUNTY, 0.0);
            let event = match death_event(&snapshot, system, bounty) { Ok(event) => event, Err(error) => { release(&world, system, &snapshot).await; return Err(error); } };
            let result = world.with(system, move |s| {
                if let Some(context) = s.npc_death_context(item, token, generation) { return Ok(Some(context)); }
                if s.combat_phase(item) != Some(crate::space::combat::CombatPhase::Dying { token })
                    || s.combat_membership_generation(item) != Some(generation) { return Ok(None); }
                let ball = s.sampled_ball(item).ok_or_else(|| anyhow::anyhow!("NPC ball unavailable"))?;
                let frame = crate::space::snapshot::capture_fact_checkpoint(s).map_err(|error| anyhow::anyhow!("NPC fact checkpoint refused: {error:?}"))?;
                let context = Arc::new(NpcDeathContext { frame, event, ball, had_unknown_commit: AtomicBool::new(false) });
                Ok(s.install_npc_death_context(item, token, generation, context.clone()).then_some(context))
            }).await.ok_or_else(|| CallError::Internal(anyhow::anyhow!("NPC death actor unavailable"))).and_then(|result| result.map_err(CallError::Internal));
            match result { Ok(context) => context, Err(error) => { release(&world, system, &snapshot).await; return Err(error); } }
        }
    };
    let Some(context) = context else { return Ok(()) };
    let key = death_key(&snapshot);
    let stored = match crate::dao::progress::source(&db, "business", key.as_str()).await {
        Ok(stored) => stored,
        Err(error) => { release(&world, system, &snapshot).await; return Err(db_err(error)); }
    };
    if let Some(stored) = stored {
        let result = async {
            let (at, expected_context) = crate::lua::events::combat::metadata(&context.event).expect("NPC death metadata");
            let actual_context: serde_json::Value = serde_json::from_str(&stored.context).map_err(|error| CallError::Internal(error.into()))?;
            if stored.event != context.event || stored.occurred_at != at || stored.ordinal != 0
                || actual_context != serde_json::to_value(expected_context).map_err(|error| CallError::Internal(error.into()))?
                || stored.origin != expected_context.origin.as_str()
                || stored.subject_character_id != crate::lua::events::combat::subject(&context.event) {
                return Err(CallError::Internal(anyhow::anyhow!("NPC death source collision")));
            }
            if let Some(frame) = &context.frame {
                if !frame.is_committed(&db).await.map_err(db_err)? {
                    return Err(CallError::Internal(anyhow::anyhow!("NPC death checkpoint incomplete")));
                }
            }
            finalize(&world, system, &snapshot, context.frame.clone(), std::collections::HashSet::new()).await?;
            Ok(())
        }.await;
        if result.is_err() { release(&world, system, &snapshot).await; }
        return result;
    }
    // A later gated checkpoint can already have committed this frame and newer HP.
    // Replaying its rows then would overwrite that newer durable condition.
    let frame_committed = if let Some(frame) = &context.frame {
        match frame.is_committed(&db).await {
            Ok(committed) => committed,
            Err(error) => { release(&world, system, &snapshot).await; return Err(db_err(error)); }
        }
    } else { false };
    let result = db.transaction(
        |tx, outbox: &mut Outbox<DeathEffect>| {
            let context = context.clone();
            let snapshot = snapshot.clone();
            Box::pin(async move {
                let written = if frame_committed {
                    if let Some(frame) = &context.frame { frame.append(outbox); }
                    Vec::new()
                } else {
                    super::super::checkpoint_conditions_in(tx, outbox, system, &[], context.frame.as_deref()).await?
                };
                if let Some(damage) = &snapshot.pending_damage { damage.append(outbox); }
                outbox.event_from(death_key(&snapshot), context.event.clone());
                if let GameEvent::NpcDestroyed { killer_id: Some(char_id), .. } = &context.event {
                    outbox.event_from(SourceKey::canonical("NpcKilled", death_key(&snapshot).as_str(), 0),
                        GameEvent::NpcKilled { char_id: *char_id, npc_id: item, type_id: context.ball.type_id, system_id: system });
                }
                outbox.operation(DeathEffect { key: death_key(&snapshot).as_str().to_owned(), fresh: false });
                Ok::<_, CallError>(written)
            })
        },
        |written, outbox| async {
            let removed = finalize(&world, system, &snapshot, context.frame.clone(), written.into_iter().collect()).await?;
            let mut fresh = false;
            for effect in outbox.into_entries() {
                match effect {
                    crate::db::outbox::Effect::Notification(notification) => notification.dispatch(&world, None)?,
                    crate::db::outbox::Effect::Operation(effect) => fresh = effect.fresh,
                }
            }
            if removed && fresh {
                let killer = match &context.event { GameEvent::NpcDestroyed { killer_id, .. } => *killer_id, _ => None };
                finish_npc_kill(&db, &world, system, item, context.ball.clone(), killer).await?;
            }
            Ok(())
        },
    ).await;
    if let Err(error) = &result {
        use crate::db::transaction::TransactionError;
        if error.is_retryable_transaction() && !context.had_unknown_commit.load(Ordering::SeqCst) {
            match crate::dao::progress::source(&db, "business", key.as_str()).await {
                Ok(None) => {
                    world.with(system, move |s| { s.restore_known_rollback(item, token, generation); }).await;
                }
                _ => release(&world, system, &snapshot).await,
            }
        } else {
            context.had_unknown_commit.store(true, Ordering::SeqCst);
            release(&world, system, &snapshot).await;
        }
    }
    result
}

#[cfg(test)] mod tests;
