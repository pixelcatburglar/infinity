//! The shared durable checkpoint receipt for NPC bounty pool rows.
//!
//! One `Arc<Checkpoint>` authorizes exactly one absolute write of its captured
//! rows: the periodic snapshot, the dock/transfer/logout boundaries and the
//! payout's final capture all write through `write_in` inside the caller's
//! transaction and prove the outcome later through `resolve_in`. The Arc
//! outlives its caller's Future; a cached `Unknown` is re-probed under a fresh
//! character fence, while other outcomes remain authoritative.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use anyhow::anyhow;
use crate::client::filetime_now;
use crate::dao::{characters::selection::lock_character, jobs, npc_bounties::{self, Pool}};
use crate::db::Transaction;
use crate::poison::lock_or_exit as lock;
use crate::services::{db_err, CallError};
use crate::space::World;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointOutcome {
    /// Every captured row plus its payout job is durably present.
    Durable,
    /// The write provably did not commit: every row still equals the recorded
    /// before-image. The bucket keeps its dirty bit and may capture again.
    Unchanged,
    /// Partial, contradictory or unreadable state; the receipt stays pending.
    Unknown,
}

/// One captured row: absolute pool values, the revision the ack may advance
/// and the actor incarnation that authorized this write.
pub(crate) struct Capture {
    pub pool: Pool,
    pub revision: u64,
    pub incarnation: Arc<()>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Status {
    Prepared,
    InFlight,
    /// The frozen payout owner reissued the same captured write after a
    /// cached `Unknown`; a before-image match can never be `Unchanged`.
    RetryInFlight,
    Durable,
    Unchanged,
    Unknown,
    /// A paid batch's leftover writers can never write again.
    Retired,
}

pub(crate) struct Checkpoint {
    pub(crate) captures: Vec<Capture>,
    /// Before-image for the capture keys, recorded inside the write
    /// transaction before the first pool write; retaken on every retry.
    preimage: Mutex<Vec<Option<Pool>>>,
    outcome: Mutex<Option<CheckpointOutcome>>,
    status: Mutex<Status>,
}

impl Checkpoint {
    pub(crate) fn new(captures: Vec<Capture>) -> Self {
        Self { captures, preimage: Mutex::new(Vec::new()), outcome: Mutex::new(None), status: Mutex::new(Status::Prepared) }
    }

    /// Characters this checkpoint writes, ascending: the row-lock order every
    /// write and resolve transaction shares.
    fn characters(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.captures.iter().map(|capture| capture.pool.batch.character_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Absolute write inside the caller's transaction: character rows ascend,
    /// the complete before-image lands on this Arc before the first pool
    /// write, then one batch INSERT plus one `ensure_once` job per batch.
    /// Internal transaction retries and the owner's `RetryInFlight` attempt
    /// re-enter and retake the before-image; a finished or retired epoch can
    /// never write again.
    pub(crate) async fn write_in(&self, tx: &Transaction<'_>) -> Result<(), CallError> {
        {
            let mut status = lock(self.status.lock());
            match *status {
                Status::Prepared => *status = Status::InFlight,
                Status::InFlight | Status::RetryInFlight => {}
                _ => return Err(CallError::Internal(anyhow!("npc bounty checkpoint epoch is finished"))),
            }
        }
        for character in self.characters() {
            lock_character(tx, character).await.map_err(db_err)?;
        }
        {
            // Group captures by character so each character is read once.
            let mut preimage = Vec::with_capacity(self.captures.len());
            let mut read: HashMap<i64, Vec<Pool>> = HashMap::new();
            for capture in &self.captures {
                let character = capture.pool.batch.character_id;
                if !read.contains_key(&character) {
                    read.insert(character, npc_bounties::for_character(tx, character).await.map_err(db_err)?);
                }
                let before = read[&character].iter().find(|row| row.system_id == capture.pool.system_id).cloned();
                preimage.push(before);
            }
            *lock(self.preimage.lock()) = preimage;
        }
        let pools: Vec<Pool> = self.captures.iter().map(|capture| capture.pool.clone()).collect();
        npc_bounties::save_absolute(tx, &pools).await.map_err(db_err)?;
        let now = filetime_now();
        let mut batches: Vec<npc_bounties::Batch> = self.captures.iter().map(|capture| capture.pool.batch).collect();
        batches.sort_unstable_by_key(|batch| (batch.character_id, batch.started_at));
        batches.dedup();
        for batch in batches {
            jobs::ensure_once(tx, &super::job(batch), now).await.map_err(db_err)?;
        }
        Ok(())
    }

    /// Prove the write's outcome inside a fresh transaction. The character row
    /// lock fences any earlier write transaction that is still open, so a row
    /// set can never be misread as rolled back while its commit is pending.
    /// A cached terminal outcome returns immediately; a cached `Unknown`
    /// re-proves under a fresh fence on every call.
    pub(crate) async fn resolve_in(&self, tx: &Transaction<'_>) -> Result<CheckpointOutcome, CallError> {
        if let Some(outcome) = *lock(self.outcome.lock()) {
            if !matches!(outcome, CheckpointOutcome::Unknown) { return Ok(outcome); }
        }
        for character in self.characters() {
            lock_character(tx, character).await.map_err(db_err)?;
        }
        let status = *lock(self.status.lock());
        if matches!(status, Status::Prepared) {
            // The writer never entered its transaction: no pool write happened.
            let outcome = CheckpointOutcome::Unchanged;
            *lock(self.outcome.lock()) = Some(outcome);
            return Ok(outcome);
        }
        let preimage = lock(self.preimage.lock()).clone();
        let mut rows_by_character: HashMap<i64, Vec<Pool>> = HashMap::new();
        for capture in &self.captures {
            let character = capture.pool.batch.character_id;
            if !rows_by_character.contains_key(&character) {
                rows_by_character.insert(character, npc_bounties::for_character(tx, character).await.map_err(db_err)?);
            }
        }
        // Durable compares every captured pool against the after-image (the
        // preimage is not needed); unchanged compares every after-image
        // against its before-image, so a short preimage disqualifies it.
        let mut durable = true;
        let mut unchanged = preimage.len() == self.captures.len();
        for capture in &self.captures {
            let after = rows_by_character[&capture.pool.batch.character_id]
                .iter().find(|row| row.system_id == capture.pool.system_id);
            if after != Some(&capture.pool) { durable = false; }
        }
        if unchanged {
            for (capture, before) in self.captures.iter().zip(preimage.iter()) {
                let after = rows_by_character[&capture.pool.batch.character_id]
                    .iter().find(|row| row.system_id == capture.pool.system_id);
                if after != before.as_ref() { unchanged = false; break; }
            }
        }
        if durable {
            for batch in {
                let mut batches: Vec<npc_bounties::Batch> = self.captures.iter().map(|capture| capture.pool.batch).collect();
                batches.sort_unstable_by_key(|batch| (batch.character_id, batch.started_at));
                batches.dedup();
                batches
            } {
                let job = super::job(batch);
                match jobs::lock_job(tx, &job.id).await.map_err(db_err)? {
                    Some(stored) if stored.kind == job.kind && stored.payload == job.payload => {}
                    _ => { durable = false; break; }
                }
            }
        }
        // A retried write whose rows still equal its before-image stays
        // `Unknown`: only a fresh `Durable` proof accepts the owner retry.
        let outcome = if durable { CheckpointOutcome::Durable }
            else if unchanged && !matches!(status, Status::RetryInFlight) { CheckpointOutcome::Unchanged }
            else { CheckpointOutcome::Unknown };
        {
            let mut status = lock(self.status.lock());
            *status = match outcome {
                CheckpointOutcome::Durable => Status::Durable,
                CheckpointOutcome::Unchanged => Status::Unchanged,
                CheckpointOutcome::Unknown => Status::Unknown,
            };
        }
        *lock(self.outcome.lock()) = Some(outcome);
        Ok(outcome)
    }

    /// Postcommit: mark the receipt durable, advance the actors' saved
    /// revisions and release their writer holds, then wake the scheduler.
    pub(crate) async fn acknowledge(&self, world: &World) -> Result<(), CallError> {
        {
            *lock(self.status.lock()) = Status::Durable;
            *lock(self.outcome.lock()) = Some(CheckpointOutcome::Durable);
        }
        self.apply(world, CheckpointOutcome::Durable).await;
        crate::scheduler::wake();
        Ok(())
    }

    /// Permanently end this write authorization: a settled batch's leftover
    /// receipt can never resurrect its rows. The cached outcome stays.
    pub(crate) fn retire(&self) {
        *lock(self.status.lock()) = Status::Retired;
    }

    /// Owner-only retry after a cached `Unknown`: the frozen payout reissues
    /// the same captured absolute write. The stale before-image and outcome
    /// are cleared so `write_in` re-records them and the next fresh
    /// `resolve_in` must prove `Durable`; a rollback, partial write or
    /// before-image match can never downgrade this attempt to `Unchanged`.
    pub(crate) async fn retry_write_in(&self, tx: &Transaction<'_>) -> Result<(), CallError> {
        {
            let mut status = lock(self.status.lock());
            match *status {
                Status::Unknown | Status::RetryInFlight => *status = Status::RetryInFlight,
                _ => return Err(CallError::Internal(anyhow!("npc bounty checkpoint retry requires an unknown outcome"))),
            }
            lock(self.preimage.lock()).clear();
            *lock(self.outcome.lock()) = None;
        }
        self.write_in(tx).await
    }

    /// Record a resolved outcome on the Arc and deliver it to the owning
    /// actors. `Unknown` keeps the writer hold so no second writer is issued;
    /// `Durable` and `Unchanged` release it.
    pub(crate) async fn settle(&self, world: &World, outcome: CheckpointOutcome) -> Result<(), CallError> {
        {
            let mut status = lock(self.status.lock());
            *status = match outcome {
                CheckpointOutcome::Durable => Status::Durable,
                CheckpointOutcome::Unchanged => Status::Unchanged,
                CheckpointOutcome::Unknown => Status::Unknown,
            };
            *lock(self.outcome.lock()) = Some(outcome);
        }
        self.apply(world, outcome).await;
        Ok(())
    }

    /// Deliver the outcome per captured row; a missing actor simply has no
    /// live bucket left to acknowledge.
    async fn apply(&self, world: &World, outcome: CheckpointOutcome) {
        let identity = self as *const Self as usize;
        for capture in &self.captures {
            let character = capture.pool.batch.character_id;
            let batch = capture.pool.batch;
            let revision = capture.revision;
            let incarnation = capture.incarnation.clone();
            world.with(capture.pool.system_id, move |s|
                s.apply_bounty_receipt(character, identity, batch, revision, incarnation, outcome)
            ).await;
        }
    }
}
