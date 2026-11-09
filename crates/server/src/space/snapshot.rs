//! Periodic persistent ship and drone position, condition and combat fact checkpoints
//! for destiny's ballpark (michelle.py). Uses game.items, item_attributes and the
//! progress journal on the system worker, including shutdown flushes; the interval
//! is operator-configured. Other ball state and persistent timers are not supported.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

mod facts;
pub(crate) use facts::{capture_fact_checkpoint, FactCheckpoint};
#[cfg(test)] pub(crate) use facts::facts_test_init;

#[derive(Clone)]
pub(super) struct Context {
    pub tx: mpsc::WeakUnboundedSender<actor::Job>,
    pub gate: Arc<tokio::sync::Mutex<()>>,
    pub written: Arc<AtomicU64>,
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
    position_ids: Vec<i64>,
    condition_rows: Vec<(i64, [f64; 3], i64)>,
    attribute_ids: [i32; 3],
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
    let handle = SystemHandle { tx, gate: ctx.gate.clone(), written: ctx.written.clone() };
    let db = ctx.db.clone();
    s.defer(async move {
        let _guard = guard;
        if let Err(error) = write_snapshot(&handle, &db).await {
            tracing::warn!(%error, "space snapshot failed");
        }
    });
}

pub(super) async fn flush(handle: SystemHandle, db: Db) -> Result<u64> {
    let before = handle.written.load(Ordering::SeqCst);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let worker_handle = handle.clone();
    handle.send(Box::new(move |s| s.defer(async move {
        let _guard = worker_handle.gate.lock().await;
        let result = write_snapshot(&worker_handle, &db).await
            .map(|_| worker_handle.written.load(Ordering::SeqCst) - before);
        let _ = tx.send(result);
    }))).map_err(|_| anyhow::anyhow!("system actor stopped before position flush"))?;
    rx.await.map_err(|_| anyhow::anyhow!("position flush worker unavailable"))?
}

async fn write_snapshot(handle: &SystemHandle, db: &Db) -> Result<u64> {
    let (system, positions, conditions, facts) = handle.with(|s| {
        let (facts, at) = facts::prepare_fact_checkpoint(s).map_err(|error| anyhow::anyhow!("combat fact capture refused: {error:?}"))?;
        let positions = candidates(s).filter_map(|(id, generation)| {
            s.ball(id).map(|ball| Position { id, generation, pos: ball.pos })
        }).collect::<Vec<_>>();
        let conditions = s.persistent_balls()
            .filter_map(|ball| s.capture_dirty_condition(ball.id, true, at))
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>((s.id, positions, conditions, facts))
    }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped during space snapshot"))??;
    if positions.is_empty() && conditions.is_empty() && facts.is_none() { return Ok(0); }

    let position_ids: Vec<_> = positions.iter().map(|p| p.id).collect();
    let condition_rows: Vec<_> = conditions.iter().map(|c|
        (c.item_id, [c.shield, c.armor_damage, c.hull_damage], c.sampled_at)
    ).collect();
    let attribute_ids = [
        infinity_core::consts::dogma::ATTR_SHIELD_CHARGE,
        infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE,
        infinity_core::consts::dogma::ATTR_HULL_DAMAGE,
    ];
    let batch = Arc::new(SnapshotBatch { system, positions, conditions, facts, position_ids, condition_rows, attribute_ids });
    let (written_positions, written_conditions) = db.transaction::<_, _, crate::db::Error, crate::db::outbox::Outbox<()>, _, _, _>(
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
                Ok((written_positions, written_conditions))
            })
        },
        |result, _| async move { Ok(result) },
    ).await?;
    let SnapshotBatch { positions, conditions, facts, .. } = Arc::try_unwrap(batch)
        .map_err(|_| anyhow::anyhow!("space snapshot batch still shared after transaction"))?;
    let count = written_positions.len() as u64;
    handle.written.fetch_add(count, Ordering::SeqCst);
    handle.with(move |s| {
        acknowledge(s, positions, &written_positions);
        let written: std::collections::HashSet<_> = written_conditions.into_iter().collect();
        for condition in conditions {
            if written.contains(&condition.item_id) {
                s.acknowledge_condition(condition.item_id, condition.generation, condition.revision);
            }
        }
        if let Some(facts) = facts { facts.acknowledge(s, &written); }
    }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped acknowledging space snapshot"))?;
    Ok(count)
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

#[cfg(test)] mod tests;
