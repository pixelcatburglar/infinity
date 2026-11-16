//! NPC bounty pools: per-actor live authority plus shared batch coordination.
//!
//! Each system actor owns the authoritative amount/count for its pilots;
//! the shared `Coordinator` holds only batch identity, freeze/payout owner and
//! the source-system set a payout must gate. Checkpoint rows in
//! `game.npc_bounty_pools` make the unpaid part durable; the payout job row
//! rides the same checkpoint transaction.
//!
//! Lock order (lock graph, kept next to the code that depends on it):
//! - kill: actor turn -> short shared `meta` mutex. No market guard, no system
//!   gate, no async character guard; the sync mutex never crosses an await.
//! - snapshot/boundary capture: held system gate -> actor turn -> `meta`.
//! - payout: market -> async character payout guard -> source gates ascending
//!   -> `meta` (short) -> DB character row -> pool/economy.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use marshal::Value;
use infinity_core::consts::FILETIME_SECOND;
use crate::dao::npc_bounties::{self, Batch, Pool};
use crate::isk::IskAmount;
use crate::poison::lock_or_exit as lock;
use crate::services::{bounty::npc::checkpoint::{Capture, Checkpoint, CheckpointOutcome}, CallError};
use super::*;

/// One NPC kill recorded while the character's batch was frozen for payout;
/// kept per-actor so the deferred notice can keep its own type.
#[derive(Clone, Copy)]
pub(in crate::space) struct PendingKill {
    pub amount: IskAmount,
    pub enemy_type: i32,
}

/// Live pool authority for one character in this system.
pub(in crate::space) struct Bucket {
    pub batch: Batch,
    pub amount: IskAmount,
    pub kill_count: i64,
    pub revision: u64,
    pub saved_revision: u64,
    /// A captured writer not yet resolved; while set no second writer is issued.
    pub checkpoint: Option<Arc<Checkpoint>>,
    pub pending: Vec<PendingKill>,
}

impl Bucket {
    fn new(batch: Batch) -> Self {
        Self { batch, amount: IskAmount::ZERO, kill_count: 0, revision: 1, saved_revision: 1, checkpoint: None, pending: Vec::new() }
    }
    fn restored(row: &Pool) -> Self {
        Self { batch: row.batch, amount: row.amount, kill_count: row.kill_count, revision: 1, saved_revision: 1, checkpoint: None, pending: Vec::new() }
    }
}

/// Per-system NPC bounty state; `incarnation` fences receipts issued by a
/// previous incarnation of this actor.
#[derive(Default)]
pub(crate) struct State {
    pub(in crate::space) buckets: HashMap<i64, Bucket>,
    pub(in crate::space) incarnation: Arc<()>,
}

/// Shared per-character payout metadata. `meta` is a short sync mutex: batch
/// selection, source registration and pool updates complete inside one
/// critical section with no await and no World callback.
pub(crate) struct CharacterState {
    pub meta: std::sync::Mutex<Meta>,
    pub payout_guard: tokio::sync::Mutex<()>,
}

#[derive(Default)]
pub(crate) struct Meta {
    /// Current open batch identity; `None` between a paid batch and its first
    /// post-resolution kill.
    pub batch: Option<Batch>,
    /// The payout owning the frozen batch, while it is frozen.
    pub frozen: Option<Arc<crate::services::bounty::npc::Payout>>,
    /// Every system that ever held a live bucket for this batch; payouts
    /// gate and enumerate from it.
    pub sources: HashSet<i64>,
    /// Aggregate-only record of kills pending on a frozen batch.
    pub pending: PendingMeta,
    /// Paid payouts kept until every captured or pending source aligned.
    pub retired: Vec<Arc<crate::services::bounty::npc::Payout>>,
}

#[derive(Default)]
pub(crate) struct PendingMeta {
    pub earliest: Option<i64>,
    pub sources: HashSet<i64>,
}

/// Process-wide ready flag and the recovered batch identities.
pub(crate) struct Coordinator {
    ready: std::sync::atomic::AtomicBool,
    init: tokio::sync::Mutex<()>,
    characters: std::sync::Mutex<HashMap<i64, Arc<CharacterState>>>,
}

impl Coordinator {
    pub(crate) fn new() -> Self {
        Self { ready: false.into(), init: tokio::sync::Mutex::new(()), characters: std::sync::Mutex::new(HashMap::new()) }
    }

    pub(crate) fn ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn lookup(&self, character: i64) -> Option<Arc<CharacterState>> {
        lock(self.characters.lock()).get(&character).cloned()
    }

    /// Existing or fresh shared state; fresh states carry no batch until the
    /// first kill or a restored row assigns one.
    pub(crate) fn character(&self, character: i64) -> Arc<CharacterState> {
        lock(self.characters.lock()).entry(character).or_insert_with(|| Arc::new(CharacterState {
            meta: std::sync::Mutex::new(Meta::default()),
            payout_guard: tokio::sync::Mutex::new(()),
        })).clone()
    }

    /// One-time recovery of every durable batch identity. Only a successful
    /// read publishes `ready`; a character carrying two different identities
    /// is a contradiction and fails instead of picking one row.
    pub(crate) async fn load(&self, db: &crate::db::Db) -> anyhow::Result<()> {
        if self.ready() { return Ok(()); }
        let _init = self.init.lock().await;
        if self.ready() { return Ok(()); }
        let batches = npc_bounties::batches(db).await?;
        {
            let mut characters = lock(self.characters.lock());
            for batch in batches {
                let state = characters.entry(batch.character_id).or_insert_with(|| Arc::new(CharacterState {
                    meta: std::sync::Mutex::new(Meta::default()),
                    payout_guard: tokio::sync::Mutex::new(()),
                })).clone();
                let mut meta = lock(state.meta.lock());
                if meta.batch.is_some_and(|stored| stored != batch) {
                    anyhow::bail!("contradictory npc bounty batches for character {}", batch.character_id);
                }
                meta.batch = Some(batch);
            }
        }
        self.ready.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

fn interval_ticks() -> Option<i64> {
    i64::from(crate::config::game().combat.bounty_payout_interval_secs).checked_mul(FILETIME_SECOND)
}

fn notice(amount: IskAmount, due_at: i64, enemy_type: i32) -> Vec<Value> {
    vec![Value::dict([
        ("amount", Value::Float(amount.wire_isk())),
        ("payoutTime", Value::Int(due_at)),
        ("enemyTypeID", Value::Int(i64::from(enemy_type))),
        ("isModified", Value::Bool(false)),
    ])]
}

fn overflow() -> CallError {
    CallError::Internal(anyhow::anyhow!("npc bounty pool arithmetic overflow"))
}

/// The character's current batch, creating it from the earliest pending kill
/// or this kill when none exists yet. `None` when nothing supplies a start.
fn ensure_batch_locked(meta: &mut Meta, character: i64, earliest: Option<i64>) -> Result<Option<Batch>, CallError> {
    if let Some(batch) = meta.batch { return Ok(Some(batch)); }
    let Some(started) = earliest else { return Ok(None) };
    let due = started.checked_add(interval_ticks().ok_or_else(overflow)?).ok_or_else(overflow)?;
    meta.batch = Some(Batch { character_id: character, started_at: started, due_at: due });
    Ok(meta.batch)
}

/// Drain pending kills into `batch`, updating the bucket's absolute values.
/// A batch identity change resets the bucket: a paid batch's amount must
/// never leak into its successor. Returns the accepted kills; an arithmetic
/// refusal leaves the rest pending.
fn drain_pending_locked(bucket: &mut Bucket, batch: Batch) -> Vec<PendingKill> {
    if bucket.batch != batch {
        bucket.batch = batch;
        bucket.amount = IskAmount::ZERO;
        bucket.kill_count = 0;
    }
    let mut accepted = 0;
    for kill in &bucket.pending {
        match (bucket.amount.checked_add(kill.amount), bucket.kill_count.checked_add(1), bucket.revision.checked_add(1)) {
            (Some(amount), Some(count), Some(revision)) => {
                (bucket.amount, bucket.kill_count, bucket.revision) = (amount, count, revision);
            }
            _ => break,
        }
        accepted += 1;
    }
    bucket.pending.drain(..accepted).collect()
}

impl System {
    fn bounty_shared(&self, character: i64) -> Option<Arc<CharacterState>> {
        self.world.upgrade().map(|world| world.bounties.character(character))
    }
    fn bounty_shared_lookup(&self, character: i64) -> Option<Arc<CharacterState>> {
        self.world.upgrade().and_then(|world| world.bounties.lookup(character))
    }

    /// One kill's rounded gross joins the pilot's live pool or, while the
    /// shared batch is frozen for payout, the pending list. The pool notice is
    /// deferred while frozen; kills never touch the database, the market lock
    /// or a per-kill job write.
    pub(crate) fn add_npc_bounty(&mut self, character: i64, enemy_type: i32, amount: IskAmount, occurred_at: i64) -> Result<(), CallError> {
        let shared = self.bounty_shared(character)
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("world stopped during npc bounty")))?;
        let drained;
        let deadline = {
            let mut meta = lock(shared.meta.lock());
            meta.sources.insert(self.id);
            if meta.frozen.is_some() {
                meta.pending.sources.insert(self.id);
                if meta.pending.earliest.is_none_or(|earliest| occurred_at < earliest) {
                    meta.pending.earliest = Some(occurred_at);
                }
                self.bounties.buckets.entry(character).or_insert_with(|| Bucket::new(Batch { character_id: character, started_at: occurred_at, due_at: occurred_at.saturating_add(1) }))
                    .pending.push(PendingKill { amount, enemy_type });
                return Ok(());
            }
            drained = self.align_npc_bounty_locked(character, Some(occurred_at), &mut meta)?;
            let batch = meta.batch.ok_or_else(overflow)?;
            let bucket = self.bounties.buckets.entry(character).or_insert_with(|| Bucket::new(batch));
            let (amount2, count, revision) = (
                bucket.amount.checked_add(amount).ok_or_else(overflow)?,
                bucket.kill_count.checked_add(1).ok_or_else(overflow)?,
                bucket.revision.checked_add(1).ok_or_else(overflow)?,
            );
            (bucket.amount, bucket.kill_count, bucket.revision) = (amount2, count, revision);
            batch.due_at
        };
        for kill in drained {
            self.notify_pilot(character, "OnBountyAddedToPayout", notice(kill.amount, deadline, kill.enemy_type));
        }
        self.notify_pilot(character, "OnBountyAddedToPayout", notice(amount, deadline, enemy_type));
        Ok(())
    }

    /// Publish a payout's batch decision into this actor: drain the pending
    /// kills into the decided batch and queue their deferred notices. Runs
    /// lazily on the next kill too, so an aligned epoch can never strand kills.
    /// Returns whether this actor turn actually completed the alignment.
    pub(crate) fn align_npc_bounty(&mut self, character: i64) -> bool {
        let Some(shared) = self.bounty_shared_lookup(character) else { return false };
        let Some((notified, due)) = ({
            let mut meta = lock(shared.meta.lock());
            if meta.frozen.is_some() { return false; }
            match self.align_npc_bounty_locked(character, meta.pending.earliest, &mut meta) {
                // A paid batch with no successor still completed its alignment.
                Ok(notified) => Some((notified, meta.batch.map(|batch| batch.due_at))),
                Err(_) => None,
            }
        }) else { return false };
        if let Some(due) = due {
            for kill in notified {
                self.notify_pilot(character, "OnBountyAddedToPayout", notice(kill.amount, due, kill.enemy_type));
            }
        }
        true
    }

    /// Drain pending kills into `meta.batch` inside the caller's meta
    /// critical section; returns the accepted kills for deferred notices.
    /// Eager and lazy alignment share this step: the paid epoch retires on
    /// it whether or not a successor batch opened, so its retired owner —
    /// and the source gates it holds — can drop.
    fn align_npc_bounty_locked(&mut self, character: i64, earliest: Option<i64>, meta: &mut Meta) -> Result<Vec<PendingKill>, CallError> {
        let drained = match ensure_batch_locked(meta, character, earliest)? {
            Some(batch) => {
                let bucket = self.bounties.buckets.entry(character).or_insert_with(|| Bucket::new(batch));
                drain_pending_locked(bucket, batch)
            }
            // A paid batch with no pending kill has no successor to open;
            // alignment still completes so the retired payout is released.
            None => Vec::new(),
        };
        meta.retained_mark_aligned(self.id);
        Ok(drained)
    }

    /// Restore one system's durable rows at cold load; restored rows are clean
    /// and their batch must not contradict the recovered shared identity.
    pub(in crate::space) fn restore_bounties(&mut self, rows: &[Pool]) {
        let Some(world) = self.world.upgrade() else { return };
        for row in rows {
            let shared = world.bounties.character(row.batch.character_id);
            {
                let mut meta = lock(shared.meta.lock());
                // A row read before a paid batch's delete commits must not
                // resurrect it: settled identities stay refused forever.
                if meta.retired.iter().any(|payout| payout.batch() == row.batch) { continue; }
                if meta.batch.is_some_and(|stored| stored != row.batch) {
                    tracing::warn!(character = row.batch.character_id, "npc bounty batch identity conflict during restore");
                    continue;
                }
                meta.batch = Some(row.batch);
                meta.sources.insert(self.id);
            }
            self.bounties.buckets.insert(row.batch.character_id, Bucket::restored(row));
        }
    }

    /// Receipts still unresolved for these rows; callers resolve each before
    /// issuing a new writer for the same rows.
    pub(crate) fn bounty_pending_receipts(&self, character: Option<i64>) -> Vec<Arc<Checkpoint>> {
        let mut seen = HashSet::new();
        let mut receipts = Vec::new();
        for (&char_id, bucket) in &self.bounties.buckets {
            if character.is_some_and(|wanted| wanted != char_id) { continue; }
            if let Some(checkpoint) = &bucket.checkpoint {
                if seen.insert(Arc::as_ptr(checkpoint)) { receipts.push(checkpoint.clone()); }
            }
        }
        receipts
    }

    /// Capture dirty, unfrozen buckets under the held system gate; `Some`
    /// limits the capture to one character's rows for a boundary settlement.
    pub(crate) fn capture_npc_bounties(&mut self, character: Option<i64>) -> Result<Option<Arc<Checkpoint>>, CallError> {
        self.capture_bounties(character, false)
    }

    /// The payout's final capture: every live bucket of the frozen batch,
    /// clean buckets included, so live values absolutely overwrite durable ones.
    pub(crate) fn capture_npc_bounties_final(&mut self, character: i64) -> Result<Option<Arc<Checkpoint>>, CallError> {
        self.capture_bounties(Some(character), true)
    }

    fn capture_bounties(&mut self, character: Option<i64>, all: bool) -> Result<Option<Arc<Checkpoint>>, CallError> {
        let mut captures = Vec::new();
        for (&char_id, bucket) in self.bounties.buckets.iter() {
            if character.is_some_and(|wanted| wanted != char_id) { continue; }
            if bucket.checkpoint.is_some() { continue; }
            // A bucket whose batch is no longer the shared identity is stale:
            // capturing it would resurrect a settled batch's rows under an
            // identity `delete_batch` already removed.
            let (consistent, frozen) = match self.bounty_shared_lookup(char_id) {
                Some(shared) => {
                    let meta = lock(shared.meta.lock());
                    (meta.batch.is_some_and(|current| current == bucket.batch), meta.frozen.is_some())
                }
                None => (false, false),
            };
            if !consistent { continue; }
            let include = all || (!frozen && bucket.revision != bucket.saved_revision);
            if !include { continue; }
            captures.push(Capture {
                pool: Pool { batch: bucket.batch, system_id: self.id, amount: bucket.amount, kill_count: bucket.kill_count },
                revision: bucket.revision,
                incarnation: self.bounties.incarnation.clone(),
            });
        }
        if captures.is_empty() { return Ok(None); }
        let checkpoint = Arc::new(Checkpoint::new(captures));
        for capture in &checkpoint.captures {
            if let Some(bucket) = self.bounties.buckets.get_mut(&capture.pool.batch.character_id) {
                bucket.checkpoint = Some(checkpoint.clone());
            }
        }
        Ok(Some(checkpoint))
    }

    /// Deliver a resolved receipt to its owning bucket: `Durable` advances the
    /// saved revision to the captured one only while it is still current;
    /// `Unchanged` keeps the dirty bit; `Unknown` keeps the writer hold.
    pub(crate) fn apply_bounty_receipt(&mut self, character: i64, checkpoint: usize, batch: Batch, revision: u64, incarnation: Arc<()>, outcome: CheckpointOutcome) {
        let Some(bucket) = self.bounties.buckets.get_mut(&character) else { return };
        let owned = bucket.checkpoint.as_ref().is_some_and(|stored| Arc::as_ptr(stored) as usize == checkpoint);
        if !owned || matches!(outcome, CheckpointOutcome::Unknown) { return; }
        bucket.checkpoint = None;
        if matches!(outcome, CheckpointOutcome::Durable)
            && bucket.batch == batch
            && std::sync::Arc::ptr_eq(&self.bounties.incarnation, &incarnation)
            && bucket.revision == revision {
            bucket.saved_revision = revision;
        }
    }

    /// Any bounty obligation that must block an idle stop: a dirty bucket, an
    /// unresolved receipt or kills still waiting for a batch decision.
    pub(crate) fn bounty_checkpoint_pending(&self) -> bool {
        self.bounties.buckets.iter().any(|(&char_id, bucket)| {
            if !bucket.pending.is_empty() || bucket.checkpoint.is_some() { return true; }
            if bucket.revision == bucket.saved_revision { return false; }
            // Dirty only counts while the batch is still the shared identity;
            // a stale bucket can never write and resolves on lazy align.
            self.bounty_shared_lookup(char_id)
                .is_some_and(|shared| lock(shared.meta.lock()).batch.is_some_and(|current| current == bucket.batch))
        })
    }
}

impl Meta {
    /// Mark this system aligned on every retired payout; a fully aligned one
    /// leaves the retained set.
    fn retained_mark_aligned(&mut self, system: i64) {
        self.retired.retain(|payout| !payout.mark_aligned(system));
    }
}

impl World {
    /// Recover durable batch identities once, under the market guard the
    /// caller already holds; production actors never start before `ready`.
    pub(crate) async fn load_bounty_batches(&self, _market: &crate::services::market::economy_lock::Guard) -> anyhow::Result<()> {
        self.bounties.load(&self.db).await
    }

    /// Deliver one kill's rounded gross to the live actor; the batch deadline
    /// and pending split resolve inside the actor turn.
    pub(crate) async fn record_npc_bounty(&self, system: i64, character: i64, enemy_type: i32, amount: IskAmount, occurred_at: i64) -> Result<(), CallError> {
        self.with(system, move |s| s.add_npc_bounty(character, enemy_type, amount, occurred_at))
            .await
            .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor unavailable for npc bounty")))?
    }
}

#[cfg(all(test, feature = "tests-space"))]
mod tests;
