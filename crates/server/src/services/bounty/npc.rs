//! Durable NPC bounty batches: one unpaid batch per character with a fixed
//! payout deadline, settled by a `game.jobs` handler in one economy
//! transaction. Pool rows live in `game.npc_bounty_pools`; live amounts stay
//! with the source system's actor. Cancelled or unknown submissions keep the
//! `Payout` owner in shared state so the next claim continues instead of
//! losing ownership.
//!
//! Lock order (payout path): market -> async per-character payout guard ->
//! live source system gates ascending -> short shared `meta` mutex -> DB
//! character row -> pool/economy. Sync mutexes never cross an await; source
//! gates are attempt-local and drop before every payout DB await.
pub(crate) mod checkpoint;
#[cfg(all(test, feature = "tests-combat"))] mod tests;

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use anyhow::anyhow;
use infinity_core::consts::{ACCOUNT_CASH, OWNER_DED, REF_BOUNTY_PRIZES, REF_CORPORATION_TAX_NPC_BOUNTIES};
use serde::{Deserialize, Serialize};
use crate::client::filetime_now;
use crate::dao::{characters::selection::lock_character, npc_bounties::{self, Batch, Pool}, wallets::{accounts, journal}};
use crate::db::{outbox::{dispatch_world, Outbox}, Db};
use crate::poison::lock_or_exit as lock;
use crate::services::{corps::tax, crimewatch, db_err, CallError};
use crate::space::{bounties::{CharacterState, PendingMeta}, World};
use self::checkpoint::{Checkpoint, CheckpointOutcome};

pub(crate) const KIND: &str = "npc.bounty.payout";

fn job_id(character: i64, started: i64) -> String {
    format!("npc-bounty:{character}:{started}")
}

/// The durable job a checkpoint transaction arms for one batch; retries keep
/// the runner's rescheduled due, so recovery never re-derives it from a job row.
pub(crate) fn job(batch: Batch) -> crate::scheduler::Job {
    crate::scheduler::Job {
        id: job_id(batch.character_id, batch.started_at),
        kind: KIND.into(),
        due_at: batch.due_at,
        payload: format!("{{\"character_id\":{},\"batch_started_at\":{}}}", batch.character_id, batch.started_at),
    }
}

#[derive(Serialize, Deserialize)]
struct Payload { character_id: i64, batch_started_at: i64 }

pub(crate) fn handle(db: Db, world: Arc<World>, job: crate::scheduler::Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let payload: Payload = serde_json::from_str(&job.payload)?;
        if job.kind != KIND || job.id != job_id(payload.character_id, payload.batch_started_at) {
            anyhow::bail!("npc bounty job identity mismatch");
        }
        settle_due(&db, &world, (payload.character_id, payload.batch_started_at), filetime_now()).await
            .map_err(|error| match error {
                CallError::Internal(error) => error,
                CallError::User { msg, .. } => anyhow::anyhow!("npc bounty payout refused: {msg}"),
            })
    })
}

/// Read the character's complete pool row set under its row lock; the lock
/// doubles as the fence against an earlier open transaction.
async fn reconcile_rows(db: &Db, character: i64) -> Result<Vec<Pool>, CallError> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        lock_character(tx, character).await?;
        npc_bounties::for_character(tx, character).await
    }), |rows, ()| async move { Ok(rows) }).await.map_err(db_err)
}

/// Resolve one checkpoint receipt inside its own fenced read transaction;
/// shared by every boundary and payout proof that needs the outcome.
pub(crate) async fn resolve_receipt(db: &Db, receipt: &Arc<Checkpoint>) -> Result<CheckpointOutcome, CallError> {
    db.transaction(|tx, _: &mut ()| {
        let receipt = receipt.clone();
        Box::pin(async move { receipt.resolve_in(tx).await })
    }, |outcome, ()| async move { Ok(outcome) }).await
}

/// Owner-only retry for a receipt stuck at `Unknown`: the frozen payout
/// reissues the same captured absolute write in one fresh transaction, then
/// re-proves under the character-row fence. Only `Durable` accepts the retry;
/// rollback, a partial write or a before-image match stays `Unknown`.
async fn retry_receipt(db: &Db, receipt: &Arc<Checkpoint>) -> Result<CheckpointOutcome, CallError> {
    db.transaction(|tx, _: &mut ()| {
        let receipt = receipt.clone();
        Box::pin(async move { receipt.retry_write_in(tx).await })
    }, |(), ()| async move { Ok(()) }).await?;
    resolve_receipt(db, receipt).await
}

/// Entry point for the durable deadline: recover batch metadata once under
/// the market guard, then drive the character's payout owner. `now` is the
/// explicit due judgement, shared by production and deterministic tests.
pub(crate) async fn settle_due(db: &Db, world: &Arc<World>, batch_start: (i64, i64), now: i64) -> Result<(), CallError> {
    let market = crate::services::market::lock().await;
    world.load_bounty_batches(&market).await.map_err(db_err)?;
    let character = batch_start.0;
    let Some(shared) = world.bounties.lookup(character) else {
        // Nothing recovered for this character: a row-less job is just stale;
        // surviving rows without recovered state stay for the scheduler retry.
        return match npc_bounties::for_character(db, character).await.map_err(db_err)?.is_empty() {
            true => Ok(()),
            false => Err(db_err(anyhow!("npc bounty rows without recovered batch state"))),
        };
    };
    let _guard = shared.payout_guard.lock().await;
    let payout = {
        let mut meta = lock(shared.meta.lock());
        match meta.batch {
            Some(batch) if batch.started_at == batch_start.1 => match &meta.frozen {
                Some(frozen) => frozen.clone(),
                None => {
                    if now < batch.due_at { return Err(db_err(anyhow!("npc bounty batch is not yet due"))); }
                    // Atomic cut: the batch freezes and the payout records the
                    // live sources already registered; later kills go pending.
                    let payout = Arc::new(Payout::new(batch, meta.sources.iter().copied().collect()));
                    meta.frozen = Some(payout.clone());
                    payout
                }
            },
            // Paid-and-deleted or superseded by a new batch: stale job.
            _ => return Ok(()),
        }
    };
    Payout::drive(&payout, db, world, &shared, now).await
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase { Frozen, Prepared, Paid, Unpaid, Unknown }

/// The recoverable payout owner held by the character's shared state: fixed
/// batch and cut epoch, the final capture receipts, the proven durable
/// preimage, the armed marker and the paid/unpaid/unknown decision.
pub(crate) struct Payout {
    batch: Batch,
    cut_sources: Vec<i64>,
    phase: Mutex<Phase>,
    checkpoints: Mutex<Vec<Arc<Checkpoint>>>,
    /// Complete, proven-durable final row set; never empty once Prepared.
    preimage: Mutex<Vec<Pool>>,
    /// Set synchronously before the economy transaction is invoked; any
    /// cancellation keeps the preimage and never re-arms a second payment.
    armed: std::sync::atomic::AtomicBool,
    /// Sources still owing alignment before this record may retire.
    unaligned: Mutex<HashSet<i64>>,
    /// Sources proven durable without a live actor turn — final-captured or
    /// cold-only rows; the only absent-actor handoff a publish may accept.
    proven: Mutex<HashSet<i64>>,
    /// Sources owing pending kills at the Paid boundary; those merge only on
    /// a live actor turn, never through an absent-actor fallback.
    pending: Mutex<HashSet<i64>>,
}

impl Payout {
    pub(crate) fn new(batch: Batch, mut cut_sources: Vec<i64>) -> Self {
        cut_sources.sort_unstable();
        cut_sources.dedup();
        Self {
            batch, cut_sources,
            phase: Mutex::new(Phase::Frozen),
            checkpoints: Mutex::new(Vec::new()),
            preimage: Mutex::new(Vec::new()),
            armed: false.into(),
            unaligned: Mutex::new(HashSet::new()),
            proven: Mutex::new(HashSet::new()),
            pending: Mutex::new(HashSet::new()),
        }
    }

    fn phase(&self) -> Phase { *lock(self.phase.lock()) }
    fn set_phase(&self, phase: Phase) { *lock(self.phase.lock()) = phase; }
    fn armed(&self) -> bool { self.armed.load(std::sync::atomic::Ordering::SeqCst) }
    fn character(&self) -> i64 { self.batch.character_id }
    pub(crate) fn batch(&self) -> Batch { self.batch }

    /// A source finished aligning to this payout's resolution; `true` when no
    /// source remains, letting the retained record drop.
    pub(crate) fn mark_aligned(&self, system: i64) -> bool {
        let mut unaligned = lock(self.unaligned.lock());
        unaligned.remove(&system);
        unaligned.is_empty()
    }

    /// `system` may count as handed off without a live actor turn only when
    /// its epoch was proven durable (final-captured or cold-only) and it owes
    /// no pending kills under the supplied obligation set.
    fn absent_aligned(&self, system: i64, pending: &HashSet<i64>) -> bool {
        !pending.contains(&system) && lock(self.proven.lock()).contains(&system)
    }

    async fn drive(payout: &Arc<Self>, db: &Db, world: &Arc<World>, shared: &Arc<CharacterState>, now: i64) -> Result<(), CallError> {
        match payout.phase() {
            Phase::Paid => Self::publish(payout, world).await,
            Phase::Unpaid => { Self::thaw(payout, world).await; Err(db_err(anyhow!("npc bounty payment did not commit"))) }
            Phase::Unknown if payout.armed() => Self::reconcile(payout, db, world, shared).await,
            _ if payout.armed() => Self::reconcile(payout, db, world, shared).await,
            Phase::Prepared => Self::pay(payout, db, world, shared, now).await,
            // An unarmed Unknown is re-provable: the final checkpoint
            // fresh-reads its pending receipts and re-captures live rows.
            Phase::Frozen | Phase::Unknown => {
                Self::final_checkpoint(payout, db, world, shared).await?;
                match payout.phase() {
                    Phase::Prepared => Self::pay(payout, db, world, shared, now).await,
                    _ => Err(db_err(anyhow!("npc bounty final checkpoint is not durable"))),
                }
            }
        }
    }

    /// Two short attempts, each holding the live source gates only across
    /// actor turns: first probe and resolve older receipts — the frozen owner
    /// may retry one `Unknown` receipt — then capture all live buckets
    /// including clean ones. Every DB await runs after the attempt-local
    /// guards drop; only a `Durable` final capture assembles the complete
    /// preimage under the character-row fence.
    async fn final_checkpoint(payout: &Arc<Self>, db: &Db, world: &Arc<World>, shared: &Arc<CharacterState>) -> Result<(), CallError> {
        let character = payout.character();
        // Pending sources join the gate set: their actors hold this payout's
        // post-resolution handoff and must stay live through the capture.
        let mut sources = payout.cut_sources.clone();
        sources.extend(lock(shared.meta.lock()).pending.sources.iter().copied());
        sources.sort_unstable();
        sources.dedup();
        // Attempt A — receipt probe: gates live only across the actor turns.
        let mut receipts = Vec::new();
        let mut proven = HashSet::new();
        {
            let mut gates = Vec::new();
            for &system in &sources {
                match world.lock_condition_snapshot(system).await {
                    Ok(gate) => {
                        let Some(found) = world.with(system, move |s| s.bounty_pending_receipts(Some(character))).await else {
                            // A gated actor closed before its turn ran: the
                            // boundary is unproven and the owner stays frozen.
                            payout.set_phase(Phase::Unknown);
                            return Err(db_err(anyhow!("npc bounty source actor closed during final capture")));
                        };
                        receipts.extend(found);
                        gates.push(gate);
                    }
                    // A stopped or absent system has no live bucket; its durable
                    // rows join the preimage as cold-only rows.
                    Err(_) => { proven.insert(system); }
                }
            }
        }
        for receipt in receipts {
            let mut outcome = resolve_receipt(db, &receipt).await?;
            if matches!(outcome, CheckpointOutcome::Unknown) {
                // Owner retry: reissue the same absolute write, then re-prove;
                // only `Durable` clears an `Unknown` receipt.
                outcome = retry_receipt(db, &receipt).await?;
            }
            receipt.settle(world, outcome).await?;
            match outcome {
                CheckpointOutcome::Durable => {
                    proven.extend(receipt.captures.iter().map(|capture| capture.pool.system_id));
                }
                // The old writer never entered; the final capture below
                // issues the replacement writer for the same live bucket.
                CheckpointOutcome::Unchanged => {}
                CheckpointOutcome::Unknown => {
                    payout.set_phase(Phase::Unknown);
                    return Err(db_err(anyhow!("npc bounty pending receipt outcome is unknown")));
                }
            }
        }
        // Attempt B — fresh capture: gates drop before the write transaction
        // and every proof read.
        let mut checkpoints = Vec::new();
        {
            let mut gates = Vec::new();
            for &system in &sources {
                match world.lock_condition_snapshot(system).await {
                    Ok(gate) => {
                        match world.with(system, move |s| s.capture_npc_bounties_final(character)).await {
                            Some(result) => {
                                if let Some(checkpoint) = result? { checkpoints.push(checkpoint); }
                            }
                            // A gated actor that closed before its capture ran
                            // leaves the boundary unproven; the owner stays.
                            None if !proven.contains(&system) => {
                                payout.set_phase(Phase::Unknown);
                                return Err(db_err(anyhow!("npc bounty source actor closed during final capture")));
                            }
                            None => {}
                        }
                        gates.push(gate);
                    }
                    Err(_) => { proven.insert(system); }
                }
            }
        }
        *lock(payout.checkpoints.lock()) = checkpoints.clone();
        if !checkpoints.is_empty() {
            db.transaction(|tx, _: &mut Outbox<()>| {
                let captures = checkpoints.clone();
                Box::pin(async move {
                    for checkpoint in &captures { checkpoint.write_in(tx).await?; }
                    Ok::<_, CallError>(())
                })
            }, |(), _| async move { Ok(()) }).await?;
            for checkpoint in &checkpoints { checkpoint.acknowledge(world).await?; }
        }
        for checkpoint in &checkpoints {
            let outcome = resolve_receipt(db, checkpoint).await?;
            checkpoint.settle(world, outcome).await?;
            match outcome {
                CheckpointOutcome::Durable => {
                    proven.extend(checkpoint.captures.iter().map(|capture| capture.pool.system_id));
                }
                _ => {
                    payout.set_phase(Phase::Unknown);
                    return Err(db_err(anyhow!("npc bounty final checkpoint is not durable")));
                }
            }
        }
        // The complete proven preimage: the absolute live writes plus the
        // durable cold-only rows, read under the same row-lock fence.
        let rows = reconcile_rows(db, character).await?;
        if rows.is_empty() || rows.iter().any(|row| row.batch != payout.batch) {
            payout.set_phase(Phase::Unknown);
            return Err(db_err(anyhow!("npc bounty batch rows contradict the frozen batch")));
        }
        *lock(payout.preimage.lock()) = rows;
        *lock(payout.proven.lock()) = proven;
        payout.set_phase(Phase::Prepared);
        Ok(())
    }

    /// The single economy transaction: per source-system aggregate tax,
    /// wallet and journal postings, one security settlement, then the exact
    /// batch delete. Postcommit marks Paid synchronously before publishing.
    async fn pay(payout: &Arc<Self>, db: &Db, world: &Arc<World>, shared: &Arc<CharacterState>, now: i64) -> Result<(), CallError> {
        let preimage = lock(payout.preimage.lock()).clone();
        if preimage.is_empty() { return Err(db_err(anyhow!("npc bounty payment has no durable preimage"))); }
        // PaymentArmed before the transaction runs: any cancellation keeps the
        // frozen owner and never re-issues a credit.
        payout.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let batch = payout.batch;
        let this = payout.clone();
        let world_post = world.clone();
        let shared_post = shared.clone();
        let result = db.transaction(
            |tx, outbox: &mut Outbox| {
                let expected = preimage.clone();
                Box::pin(async move {
                    lock_character(tx, batch.character_id).await.map_err(db_err)?;
                    let rows = npc_bounties::for_character(tx, batch.character_id).await.map_err(db_err)?;
                    if rows != expected { return Err(db_err(anyhow!("npc bounty preimage changed before payment"))); }
                    if batch.due_at > now { return Err(db_err(anyhow!("npc bounty batch is not yet due"))); }
                    let mut kills: u64 = 0;
                    let mut legacy: u64 = 0;
                    let mut gain = crate::npc_security::SecurityGain::ZERO;
                    for row in &rows {
                        kills = kills.checked_add(u64::try_from(row.kill_count).map_err(|_| db_err(anyhow!("npc bounty kill count overflow")))?)
                            .ok_or_else(|| db_err(anyhow!("npc bounty kill count overflow")))?;
                        legacy = legacy.checked_add(u64::try_from(row.legacy_kill_count).map_err(|_| db_err(anyhow!("npc bounty legacy count overflow")))?)
                            .ok_or_else(|| db_err(anyhow!("npc bounty legacy count overflow")))?;
                        gain = gain.merge(row.security_gain);
                    }
                    for row in &rows {
                        let settled = tax::settle_reward(tx, outbox, batch.character_id, row.amount, None).await?;
                        let account = accounts::ensure(tx, batch.character_id, ACCOUNT_CASH).await.map_err(db_err)?;
                        journal::append(tx, account.id, settled.net, &journal::Entry {
                            actor_id: batch.character_id, entry_type_id: REF_BOUNTY_PRIZES,
                            owner_id1: OWNER_DED, owner_id2: batch.character_id,
                            reference_id: row.system_id, created_at: now,
                            description: "NPC bounty reward",
                            market_transaction_id: None, order_id: None, side: 1,
                        }).await.map_err(db_err)?;
                        if let Some((corporation, tax_amount)) = settled.corporation {
                            let corp_account = accounts::ensure(tx, corporation, ACCOUNT_CASH).await.map_err(db_err)?;
                            journal::append(tx, corp_account.id, tax_amount, &journal::Entry {
                                actor_id: batch.character_id, entry_type_id: REF_CORPORATION_TAX_NPC_BOUNTIES,
                                owner_id1: batch.character_id, owner_id2: corporation,
                                reference_id: row.system_id, created_at: now,
                                description: "Corporation tax on NPC bounty",
                                market_transaction_id: None, order_id: None, side: 1,
                            }).await.map_err(db_err)?;
                        }
                    }
                    crimewatch::settle_npc_gain(tx, outbox, batch.character_id, kills, legacy, gain).await?;
                    if npc_bounties::delete_batch(tx, batch).await.map_err(db_err)? != expected.len() as u64 {
                        return Err(db_err(anyhow!("npc bounty batch delete count mismatch")));
                    }
                    Ok(())
                })
            },
            |(), outbox| {
                let this = this.clone();
                let world = world_post.clone();
                let shared = shared_post.clone();
                async move {
                    // Paid first, synchronously: a later cancellation only
                    // repeats the publish, never the economy work.
                    Self::begin_paid(&this, &shared);
                    let _ = Self::publish(&this, &world).await;
                    dispatch_world((), outbox, &world)
                }
            },
        ).await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => match Self::reconcile(payout, db, world, shared).await {
                Ok(()) => Ok(()),
                Err(_) => Err(error),
            },
        }
    }

    /// Reconcile an armed payment attempt: briefly acquire every currently
    /// live source gate for attempt admission — including systems cold-loaded
    /// while the outcome was unknown — then release before proving from the
    /// complete row set under the character lock fence. Empty with a proven
    /// armed preimage is `Paid`; the preimage back is `Unpaid`; anything else
    /// stays `Unknown`.
    async fn reconcile(payout: &Arc<Self>, db: &Db, world: &Arc<World>, shared: &Arc<CharacterState>) -> Result<(), CallError> {
        {
            let sources: Vec<i64> = {
                let mut meta = lock(shared.meta.lock());
                meta.sources.extend(payout.cut_sources.iter().copied());
                let mut live: Vec<i64> = meta.sources.iter().copied().collect();
                live.sort_unstable();
                live.dedup();
                live
            };
            // Attempt admission only: wait out in-flight actor captures, then
            // release the gates before the fenced row read below.
            let mut gates = Vec::new();
            for system in sources {
                if let Ok(gate) = world.lock_condition_snapshot(system).await {
                    gates.push(gate);
                }
            }
            drop(gates);
        }
        let rows = reconcile_rows(db, payout.character()).await?;
        let preimage = lock(payout.preimage.lock()).clone();
        if preimage.is_empty() {
            payout.set_phase(Phase::Unknown);
            return Err(db_err(anyhow!("npc bounty payment has no durable preimage")));
        }
        if rows == preimage {
            payout.set_phase(Phase::Unpaid);
            Self::thaw(payout, world).await;
            return Err(db_err(anyhow!("npc bounty payment did not commit")));
        }
        if rows.is_empty() {
            payout.set_phase(Phase::Paid);
            Self::begin_paid(payout, shared);
            return Self::publish(payout, world).await;
        }
        payout.set_phase(Phase::Unknown);
        Err(db_err(anyhow!("npc bounty payment outcome is unknown")))
    }

    /// Publish the paid resolution inside one meta critical section: retire
    /// the frozen owner, open the next batch at the earliest pending kill and
    /// snapshot the sources that must align. Idempotent on re-entry.
    fn begin_paid(payout: &Arc<Self>, shared: &Arc<CharacterState>) {
        let mut meta = lock(shared.meta.lock());
        if !meta.frozen.as_ref().is_some_and(|frozen| Arc::ptr_eq(frozen, payout)) { return; }
        let next = meta.pending.earliest.map(|started| Batch {
            character_id: payout.character(),
            started_at: started,
            due_at: started.saturating_add(payout_interval_ticks().unwrap_or(0)),
        });
        let mut unaligned: HashSet<i64> = payout.cut_sources.iter().copied().collect();
        unaligned.extend(meta.pending.sources.iter().copied());
        *lock(payout.unaligned.lock()) = unaligned;
        // The Paid-boundary pending obligations: only a live actor turn may
        // discharge them, so the absent-actor fallback checks this snapshot.
        *lock(payout.pending.lock()) = meta.pending.sources.iter().copied().collect();
        // The next batch's source set is only the systems whose pending kills
        // open it; resolved sources leave with this record.
        meta.sources = meta.pending.sources.iter().copied().collect();
        meta.batch = next;
        meta.pending = PendingMeta::default();
        meta.frozen = None;
        meta.retired.push(payout.clone());
    }

    /// Deliver the resolution to every still-unaligned live source; each actor
    /// merges its pending kills into the decided batch and emits the deferred
    /// notices. A stopped or closed actor completes the handoff only for a
    /// proven-durable source with no pending obligation; everything unproven
    /// stays unaligned until a later live actor turn completes the handoff.
    async fn publish(payout: &Arc<Self>, world: &World) -> Result<(), CallError> {
        let sources: Vec<i64> = lock(payout.unaligned.lock()).iter().copied().collect();
        let pending = lock(payout.pending.lock()).clone();
        let character = payout.character();
        for system in sources {
            match world.with(system, move |s| s.align_npc_bounty(character)).await {
                // The actor turn itself aligned and marked every retained payout.
                Some(true) => {}
                None if payout.absent_aligned(system, &pending) => { payout.mark_aligned(system); }
                _ => {}
            }
        }
        // Release the retained owner and end the settled batch's leftover
        // writers only once every source aligned.
        if lock(payout.unaligned.lock()).is_empty() {
            if let Some(shared) = world.bounties.lookup(character) {
                lock(shared.meta.lock()).retired.retain(|old| !Arc::ptr_eq(old, payout));
            }
            for checkpoint in lock(payout.checkpoints.lock()).iter() {
                checkpoint.retire();
            }
        }
        Ok(())
    }

    /// Prove `Unpaid`: pending kills merge back into the unchanged batch on a
    /// live actor turn; a proven-absent source has no live bucket left. Any
    /// unproven handoff restores the frozen owner and the pending aggregate
    /// so the scheduler's ordinary retry re-drives this payout.
    async fn thaw(payout: &Arc<Self>, world: &World) {
        let character = payout.character();
        let Some(shared) = world.bounties.lookup(character) else { return };
        let (sources, pending) = {
            let mut meta = lock(shared.meta.lock());
            if !meta.frozen.as_ref().is_some_and(|frozen| Arc::ptr_eq(frozen, payout)) { return; }
            meta.frozen = None;
            let mut sources = payout.cut_sources.clone();
            sources.extend(meta.pending.sources.iter().copied());
            (sources, std::mem::take(&mut meta.pending))
        };
        for system in sources {
            match world.with(system, move |s| s.align_npc_bounty(character)).await {
                Some(true) => {}
                None if payout.absent_aligned(system, &pending.sources) => {}
                _ => {
                    let mut meta = lock(shared.meta.lock());
                    meta.frozen = Some(payout.clone());
                    meta.pending = pending;
                    return;
                }
            }
        }
    }
}

fn payout_interval_ticks() -> Option<i64> {
    i64::from(crate::config::game().combat.bounty_payout_interval_secs).checked_mul(infinity_core::consts::FILETIME_SECOND)
}
