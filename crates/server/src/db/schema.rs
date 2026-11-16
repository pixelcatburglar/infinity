//! Startup game-schema application and its downgrade guard.
//! This is startup database provenance, not a client wire API, and has no client
//! mirror. `game.schema_application_history` records which embedded schema each
//! start applied; its SQL stays in `dao/schema_history`, reached through the
//! transaction's uncached `Executor` with rows read by column name. The caller
//! (`Db::connect`) owns the startup transaction and its commit/rollback; this
//! helper never opens a connection or transaction of its own, and it does no
//! recurring work. It runs once per connect while `schema_on_start` is set.

use anyhow::{bail, Context, Result};
use tracing::warn;
use crate::config::deployment::DatabaseConfig;
use crate::dao::{market::migration, schema_history};
use super::{executor::Executor, Transaction};

/// Apply the idempotent game schema inside the caller's startup transaction,
/// refusing a recorded-but-superseded hash before any DDL when the guard is on.
pub(super) async fn apply(tx: &Transaction<'_>, schema: &str, cfg: &DatabaseConfig, build_commit: &str) -> Result<()> {
    if !cfg.schema_on_start {
        warn!("game schema application skipped by configuration");
        migration::validate(&tx.uncached()).await.context("validating market schema")?;
        return Ok(());
    }
    let hash = crate::hash::sha256_hex(schema.as_bytes());
    if cfg.schema_downgrade_guard {
        let seen = schema_history::preflight(&tx.uncached(), &hash).await.context("reading schema application history")?;
        // Refusal precedes the schema batch, so the recorded build and every
        // table stay untouched; an unrecorded hash is always accepted.
        if let (true, Some(latest)) = (seen.known, seen.latest) {
            if latest.schema_hash != hash {
                bail!("refusing to apply an older recorded game schema; the latest application ran build {}", latest.build_commit);
            }
        }
    }
    tx.batch_execute(schema).await.context("applying game schema")?;
    migration::migrate(&tx.uncached()).await.context("migrating market schema")?;
    schema_history::append_if_changed(&tx.uncached(), &hash, build_commit).await.context("recording schema application")?;
    Ok(())
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
