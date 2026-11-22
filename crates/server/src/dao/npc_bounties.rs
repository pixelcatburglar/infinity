//! Durable unpaid NPC bounty pool rows in `game.npc_bounty_pools`.
//! One open batch per character; each row carries one source system's absolute
//! totals, so checkpoints rewrite values instead of accumulating deltas.
//! The payout job shares the checkpoint transaction, so a live job without any
//! rows is only ever a stale claim. Callers own transactions, gates, locks and
//! the live/DB merge; resurrection prevention is the epoch protocol, not SQL.
use crate::db::{self, executor::Executor};
use crate::isk::IskAmount;
use crate::npc_security::SecurityGain;
use tokio_postgres::Row;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Batch {
    pub character_id: i64,
    pub started_at: i64,
    pub due_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Pool {
    pub batch: Batch,
    pub system_id: i64,
    pub amount: IskAmount,
    pub kill_count: i64,
    pub legacy_kill_count: i64,
    pub security_gain: SecurityGain,
}

impl Pool {
    fn from_row(row: &Row) -> Result<Self, db::Error> {
        let gain = SecurityGain::from_storage(row.get("security_gain_log"))
            .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("npc bounty security gain is not canonical")))?;
        let legacy_kill_count = row.get("legacy_kill_count");
        let kill_count = row.get("kill_count");
        if !(0..=kill_count).contains(&legacy_kill_count) {
            return Err(db::Error::Progress(anyhow::anyhow!("npc bounty legacy count exceeds its total")));
        }
        Ok(Self {
            batch: Batch {
                character_id: row.get("character_id"),
                started_at: row.get("batch_started_at"),
                due_at: row.get("due_at"),
            },
            system_id: row.get("system_id"),
            amount: super::numeric::parse_isk(row.get("amount"))?,
            kill_count, legacy_kill_count, security_gain: gain,
        })
    }
}

const COLUMNS: &str = "character_id, system_id, batch_started_at, due_at, amount::text AS amount, kill_count, legacy_kill_count, security_gain_log";

/// Every distinct open batch identity, ordered by character. Callers reject a
/// character carrying two different identities instead of picking one row.
pub(crate) async fn batches(ex: &(impl Executor + ?Sized)) -> Result<Vec<Batch>, db::Error> {
    let rows = ex.query(
        "SELECT DISTINCT character_id, batch_started_at, due_at FROM game.npc_bounty_pools ORDER BY character_id",
        &[],
    ).await?;
    Ok(rows.iter().map(|row| Batch {
        character_id: row.get("character_id"),
        started_at: row.get("batch_started_at"),
        due_at: row.get("due_at"),
    }).collect())
}

/// All pool rows in one system, ordered by character.
pub(crate) async fn in_system(ex: &(impl Executor + ?Sized), system: i64) -> Result<Vec<Pool>, db::Error> {
    let rows = ex.query(
        &format!("SELECT {COLUMNS} FROM game.npc_bounty_pools WHERE system_id = $1 ORDER BY character_id"),
        &[&system],
    ).await?;
    rows.iter().map(Pool::from_row).collect()
}

/// All pool rows of one character, ordered by system; the complete row set a
/// payment or reconciliation reasons about, not one filtered identity.
pub(crate) async fn for_character(ex: &(impl Executor + ?Sized), character: i64) -> Result<Vec<Pool>, db::Error> {
    let rows = ex.query(
        &format!("SELECT {COLUMNS} FROM game.npc_bounty_pools WHERE character_id = $1 ORDER BY system_id"),
        &[&character],
    ).await?;
    rows.iter().map(Pool::from_row).collect()
}

/// Absolute checkpoint write: every input row overwrites its key. A conflicting
/// stored identity, a regressed amount or count, or an equal count with a
/// different amount leaves that row unwritten, and a partial write fails the
/// caller's transaction instead of acknowledging silently. Callers must already
/// hold the involved character row locks and the source system gates.
pub(crate) async fn save_absolute(ex: &(impl Executor + ?Sized), rows: &[Pool]) -> Result<(), db::Error> {
    if rows.is_empty() { return Ok(()); }
    let characters: Vec<i64> = rows.iter().map(|row| row.batch.character_id).collect();
    let systems: Vec<i64> = rows.iter().map(|row| row.system_id).collect();
    let starts: Vec<i64> = rows.iter().map(|row| row.batch.started_at).collect();
    let dues: Vec<i64> = rows.iter().map(|row| row.batch.due_at).collect();
    let amounts: Vec<String> = rows.iter().map(|row| row.amount.decimal()).collect();
    let counts: Vec<i64> = rows.iter().map(|row| row.kill_count).collect();
    let legacy: Vec<i64> = rows.iter().map(|row| row.legacy_kill_count).collect();
    let gains: Vec<Option<f64>> = rows.iter().map(|row| row.security_gain.storage()).collect();
    let written = ex.query(
        "INSERT INTO game.npc_bounty_pools
            (character_id, system_id, batch_started_at, due_at, amount, kill_count, legacy_kill_count, security_gain_log)
         SELECT source.character_id, source.system_id, source.batch_started_at, source.due_at,
                source.amount::numeric, source.kill_count, source.legacy_kill_count, source.security_gain_log
         FROM UNNEST($1::bigint[], $2::bigint[], $3::bigint[], $4::bigint[], $5::text[], $6::bigint[], $7::bigint[], $8::double precision[])
             AS source(character_id, system_id, batch_started_at, due_at, amount, kill_count, legacy_kill_count, security_gain_log)
         ON CONFLICT (character_id, system_id) DO UPDATE SET
             batch_started_at = EXCLUDED.batch_started_at, due_at = EXCLUDED.due_at,
             amount = EXCLUDED.amount, kill_count = EXCLUDED.kill_count,
             legacy_kill_count = EXCLUDED.legacy_kill_count, security_gain_log = EXCLUDED.security_gain_log
         WHERE game.npc_bounty_pools.batch_started_at = EXCLUDED.batch_started_at
           AND game.npc_bounty_pools.due_at = EXCLUDED.due_at
           AND game.npc_bounty_pools.legacy_kill_count = EXCLUDED.legacy_kill_count
           AND game.npc_bounty_pools.amount <= EXCLUDED.amount
           AND game.npc_bounty_pools.kill_count <= EXCLUDED.kill_count
           AND (game.npc_bounty_pools.security_gain_log IS NULL
                AND EXCLUDED.security_gain_log IS NULL
                OR game.npc_bounty_pools.security_gain_log IS NOT NULL
                   AND (EXCLUDED.security_gain_log IS NULL
                        OR EXCLUDED.security_gain_log >= game.npc_bounty_pools.security_gain_log))
           AND (game.npc_bounty_pools.kill_count < EXCLUDED.kill_count
                OR game.npc_bounty_pools.amount = EXCLUDED.amount
                   AND game.npc_bounty_pools.security_gain_log IS NOT DISTINCT FROM EXCLUDED.security_gain_log)
         RETURNING character_id",
        &[&characters, &systems, &starts, &dues, &amounts, &counts, &legacy, &gains],
    ).await?;
    if written.len() != rows.len() {
        return Err(db::Error::Progress(anyhow::anyhow!("npc bounty pool identity changed during absolute checkpoint")));
    }
    Ok(())
}

/// Delete exactly one batch's rows. The caller knows the complete preimage;
/// a deleted count different from it fails the payment transaction.
pub(crate) async fn delete_batch(ex: &(impl Executor + ?Sized), batch: Batch) -> Result<u64, db::Error> {
    ex.execute(
        "DELETE FROM game.npc_bounty_pools WHERE character_id = $1 AND batch_started_at = $2 AND due_at = $3",
        &[&batch.character_id, &batch.started_at, &batch.due_at],
    ).await
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
