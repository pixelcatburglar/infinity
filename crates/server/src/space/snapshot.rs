//! Periodic persistent ship and drone position, condition and combat fact checkpoints
//! for destiny's ballpark (michelle.py). Uses game.items, item_attributes and the
//! progress journal on the system worker, including shutdown flushes; the interval
//! is operator-configured. A due checkpoint may set synchronous_commit off for
//! its own transaction; a final flush keeps the connection policy and returns
//! without a transaction when nothing is dirty, so it does not wait for earlier WAL.
//! Other ball state and persistent timers are not supported.
use super::*;
use std::sync::atomic::Ordering;

mod facts;
pub(crate) use facts::{capture_fact_checkpoint, FactCheckpoint};
#[cfg(all(test, feature = "tests-effects"))] pub(crate) use facts::facts_test_init;

#[derive(Clone)]
pub(super) struct Context {
    pub tx: mpsc::WeakUnboundedSender<actor::Job>,
    pub gate: Arc<tokio::sync::Mutex<()>>,
    pub state: Arc<actor::lifecycle::ActorState>,
    pub db: Db,
}

impl World {
    /// Lock the running system's checkpoint gate without loading a system.
    pub(crate) async fn lock_condition_snapshot(&self, system: i64) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let handle = self.running(system)
            .ok_or_else(|| anyhow::anyhow!("system actor unavailable for condition snapshot"))?;
        let guard = handle.gate.clone().lock_owned().await;
        if !self.running(system).is_some_and(|current| Arc::ptr_eq(&current.gate, &handle.gate)) {
            return Err(anyhow::anyhow!("system actor changed during condition snapshot lock"));
        }
        Ok(guard)
    }
}

#[derive(Clone, Copy)]
struct Position { id: i64, generation: u64, pos: V3 }

struct SnapshotBatch {
    system: i64,
    positions: Vec<Position>,
    conditions: Vec<combat::DirtyConditionCapture>,
    facts: Option<Arc<FactCheckpoint>>,
    loaded: Vec<crate::dao::items::charges::LoadedWrite>,
    /// The system's captured NPC bounty pool writer for this checkpoint.
    bounties: Option<Arc<crate::services::bounty::npc::checkpoint::Checkpoint>>,
    position_ids: Vec<i64>,
    condition_rows: Vec<(i64, [f64; 3], i64)>,
    attribute_ids: [i32; 3],
}

fn bounty_err(e: crate::services::CallError) -> anyhow::Error {
    match e {
        crate::services::CallError::Internal(e) => e,
        crate::services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
    }
}

fn candidates(s: &System) -> impl Iterator<Item = (i64, u64)> + '_ {
    s.persistent_balls().filter_map(|ball| {
        let &(generation, previous) = s.snapshot_positions.get(&ball.id)?;
        (ball.pos != previous).then_some((ball.id, generation))
    })
}

pub(super) fn schedule(s: &System) {
    let Some(ctx) = &s.snapshot_context else { return };
    let Ok(guard) = ctx.gate.clone().try_lock_owned() else { return };
    let Some(tx) = ctx.tx.upgrade() else { return };
    let handle = SystemHandle { tx, gate: ctx.gate.clone(), state: ctx.state.clone() };
    let db = ctx.db.clone();
    s.defer(async move {
        let _guard = guard;
        if let Err(error) = write_snapshot(&handle, &db, crate::db::transaction::CommitMode::LossTolerant).await {
            tracing::warn!(%error, "space snapshot failed");
        }
    });
}

pub(super) async fn flush(handle: SystemHandle, db: Db) -> Result<u64> {
    let before = handle.state.written.load(Ordering::SeqCst);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let worker_handle = handle.clone();
    handle.send(actor::Job::new(move |s| s.defer(async move {
        let guard = worker_handle.gate.lock().await;
        let result = write_snapshot(&worker_handle, &db, crate::db::transaction::CommitMode::Default).await
            .map(|_| worker_handle.state.written.load(Ordering::SeqCst) - before);
        drop(guard);
        drop(worker_handle);
        let _ = tx.send(result);
    }))).map_err(|_| anyhow::anyhow!("system actor stopped before position flush"))?;
    rx.await.map_err(|_| anyhow::anyhow!("position flush worker unavailable"))?
}

async fn write_snapshot(handle: &SystemHandle, db: &Db, mode: crate::db::transaction::CommitMode) -> Result<u64> {
    let (system, positions, conditions, facts, loaded, unknown, pending_receipts) = handle.with(|s| {
        let (facts, at) = facts::prepare_fact_checkpoint(s).map_err(|error| anyhow::anyhow!("combat fact capture refused: {error:?}"))?;
        let positions = candidates(s).filter_map(|(id, generation)| {
            s.ball(id).map(|ball| Position { id, generation, pos: ball.pos })
        }).collect::<Vec<_>>();
        let conditions = s.persistent_balls()
            .filter_map(|ball| s.capture_dirty_condition(ball.id, true, at))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>((s.id, positions, conditions, facts,
            s.capture_dirty_loaded(&[]), s.unknown_loaded_keys(), s.bounty_pending_receipts(None)))
    }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped during space snapshot"))??;
    // A stale receipt resolves before any new writer is issued for its rows.
    if !pending_receipts.is_empty() {
        let world = handle.with(|s| s.combat_world()).await.flatten()
            .ok_or_else(|| anyhow::anyhow!("world stopped during bounty snapshot"))?;
        for receipt in pending_receipts {
            let outcome = db.transaction(|tx, _: &mut ()| {
                let receipt = receipt.clone();
                Box::pin(async move { receipt.resolve_in(tx).await.map_err(|e| crate::db::Error::Progress(bounty_err(e))) })
            }, |outcome, ()| async move { Ok(outcome) }).await?;
            receipt.settle(&world, outcome).await.map_err(bounty_err)?;
        }
    }
    let bounties = handle.with(|s| s.capture_npc_bounties(None)).await
        .ok_or_else(|| anyhow::anyhow!("system actor stopped during space snapshot"))?
        .map_err(bounty_err)?;
    if positions.is_empty() && conditions.is_empty() && facts.is_none() && loaded.is_empty() && bounties.is_none() {
        // Held/unknown slots still get their fresh-reseed retry under this gate.
        if !unknown.is_empty() { reseed_unknown(&handle, db, system, unknown).await; }
        return Ok(0);
    }

    let position_ids: Vec<_> = positions.iter().map(|p| p.id).collect();
    let condition_rows: Vec<_> = conditions.iter().map(|c|
        (c.item_id, [c.shield, c.armor_damage, c.hull_damage], c.sampled_at)
    ).collect();
    let attribute_ids = [
        infinity_core::consts::dogma::ATTR_SHIELD_CHARGE,
        infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE,
        infinity_core::consts::dogma::ATTR_HULL_DAMAGE,
    ];
    // A bounty write needs the durable Default commit; a pure position
    // snapshot keeps the caller's mode untouched.
    let mode = if bounties.is_some() { crate::db::transaction::CommitMode::Default } else { mode };
    let batch = Arc::new(SnapshotBatch { system, positions, conditions, facts, loaded, bounties, position_ids, condition_rows, attribute_ids });
    let (written_positions, written_conditions, matched_loaded) = db.transaction_with_commit::<_, _, crate::db::Error, crate::db::outbox::Outbox<()>, _, _, _>(
        mode,
        |tx, outbox| {
            let batch = Arc::clone(&batch);
            Box::pin(async move {
                if let Some(facts) = &batch.facts { facts.append(outbox); }
                let tokens = if batch.position_ids.is_empty() {
                    HashMap::new()
                } else {
                    crate::dao::items::positions::versions(tx, &batch.position_ids, batch.system).await?
                };
                let written_conditions = if batch.condition_rows.is_empty() {
                    Vec::new()
                } else {
                    crate::dao::items::dogma::save_conditions(tx, batch.system, &batch.attribute_ids, &batch.condition_rows).await?
                };
                let written_positions = if batch.position_ids.is_empty() {
                    std::collections::HashSet::new()
                } else {
                    let eligible: Vec<_> = batch.positions.iter().filter(|p| tokens.contains_key(&p.id)).collect();
                    let ids: Vec<_> = eligible.iter().map(|p| p.id).collect();
                    let versions: Vec<_> = eligible.iter().map(|p| tokens[&p.id].as_str()).collect();
                    let xs: Vec<_> = eligible.iter().map(|p| p.pos[0]).collect();
                    let ys: Vec<_> = eligible.iter().map(|p| p.pos[1]).collect();
                    let zs: Vec<_> = eligible.iter().map(|p| p.pos[2]).collect();
                    // xmin changes on every row write, so later writers win without schema changes.
                    crate::dao::items::positions::save_snapshot(tx, &ids, &versions, &xs, &ys, &zs, batch.system).await?
                };
                // One absolute batch for every dirty loaded slot: UPDATE for
                // positive quantities, DELETE for emptied ones, never INSERT.
                let matched_loaded = crate::dao::items::charges::save_absolute(tx, &batch.loaded).await?;
                if let Some(bounties) = &batch.bounties {
                    bounties.write_in(tx).await.map_err(|e| crate::db::Error::Progress(bounty_err(e)))?;
                }
                Ok((written_positions, written_conditions, matched_loaded))
            })
        },
        |result, _| async move { Ok(result) },
    ).await?;
    let SnapshotBatch { positions, conditions, facts, loaded, bounties, .. } = Arc::try_unwrap(batch)
        .map_err(|_| anyhow::anyhow!("space snapshot batch still shared after transaction"))?;
    let count = written_positions.len() as u64;
    handle.state.written.fetch_add(count, Ordering::SeqCst);
    let retry = handle.with(move |s| {
        acknowledge(s, positions, &written_positions);
        let written: std::collections::HashSet<_> = written_conditions.into_iter().collect();
        for condition in conditions {
            if written.contains(&condition.item_id) {
                s.acknowledge_condition(condition.item_id, condition.generation, condition.revision);
            }
        }
        if let Some(facts) = facts { facts.acknowledge(s, &written); }
        // Compare-value clear: only rows the batch actually matched and whose
        // identity and quantity still equal the capture lose their dirty bit;
        // anything unmatched was not saved and goes held/unknown for reseed.
        let matched: std::collections::HashSet<_> = matched_loaded.into_iter().collect();
        let saved: Vec<_> = loaded.iter().filter(|row| matched.contains(&row.key())).cloned().collect();
        let missed: Vec<_> = loaded.iter().filter(|row| !matched.contains(&row.key())).map(|row| row.key()).collect();
        s.clear_saved_loaded(&saved);
        s.invalidate_held_loaded(&missed);
        s.unknown_loaded_keys()
    }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped acknowledging space snapshot"))?;
    if let Some(bounties) = bounties {
        let world = handle.with(|s| s.combat_world()).await.flatten()
            .ok_or_else(|| anyhow::anyhow!("world stopped during bounty snapshot ack"))?;
        bounties.acknowledge(&world).await.map_err(bounty_err)?;
    }
    if !retry.is_empty() { reseed_unknown(&handle, db, system, retry).await; }
    Ok(count)
}

/// Retry the narrow fresh read for held/unknown slots under this gate; a
/// failed reseed leaves the slots held for the next snapshot period.
async fn reseed_unknown(handle: &SystemHandle, db: &Db, system: i64, keys: Vec<crate::dao::items::charges::LoadedSlotKey>) {
    let Some(world) = handle.with(|s| s.combat_world()).await.flatten() else { return };
    if let Err(error) = crate::services::combat::loaded::reseed_held(db, &world, system, &keys).await {
        tracing::warn!(?error, system, "loaded snapshot reseed failed");
    }
}

fn acknowledge(s: &mut System, positions: Vec<Position>, succeeded: &std::collections::HashSet<i64>) {
    for p in positions {
        if succeeded.contains(&p.id) {
            if let Some((generation, previous)) = s.snapshot_positions.get_mut(&p.id) {
                if *generation == p.generation { *previous = p.pos; }
            }
        }
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
