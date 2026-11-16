//! Development-index points state and strict-prefix replay checkpoints.
//! Mirrors no client module; rows in game.sov_development* live independently of
//! Hub deployment and claim ownership, so losing a Hub never removes index
//! state. Callers own transactions, policy history, receipt acknowledgement,
//! Lua and World; this module never opens a transaction or touches the outbox.
//!
//! Lock order for writers touching these rows: the namespace advisory lock from
//! dao::lua_content::activations first (shared for prepared deliveries,
//! exclusive for publication), then this table's row lock through `lock`, then
//! the delivery receipt row. `ensure` is the only insert and runs outside the
//! delivery transaction so racing first receipts converge on the same stored
//! row; `lock` never inserts. No Lua, World or service call may be awaited
//! while any of these locks is held.

use crate::db::{self, executor::Executor};
use crate::dao::lua_content::activations::ActivationRow;
use crate::dao::progress::sovereignty::FactOrder;
use crate::lua::content::indices::NAMESPACE;
use super::{policy::PolicyRow, systems::SystemRow};
use tokio_postgres::Row;

const COLUMNS: &str = "system_id, military_points, industrial_points, evaluated_at, revision, activation_ordinal, trend_start_at, military_trend_base, industrial_trend_base, tail_occurred_at, tail_namespace, tail_source_key, tail_ordinal, tail_slice_id";
const CHECKPOINT_COLUMNS: &str = "system_id, checkpoint_at, military_points, industrial_points, activation_ordinal";

fn column<T: for<'a> tokio_postgres::types::FromSql<'a>>(row: &Row, prefix: &str, name: &str) -> T {
    row.get(format!("{prefix}{name}").as_str())
}

/// Points state for one solar system; `tail` regroups the last folded fact order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DevelopmentRow {
    pub system_id: i64,
    pub military_points: f64,
    pub industrial_points: f64,
    pub evaluated_at: i64,
    pub revision: i64,
    pub activation_ordinal: i64,
    pub trend_start_at: i64,
    pub military_trend_base: f64,
    pub industrial_trend_base: f64,
    pub tail: Option<FactOrder>,
}
impl DevelopmentRow {
    fn from_columns(row: &Row, prefix: &str) -> Self {
        let tail = column::<Option<i64>>(row, prefix, "tail_occurred_at").map(|occurred_at| FactOrder {
            occurred_at,
            source_namespace: column(row, prefix, "tail_namespace"),
            source_key: column(row, prefix, "tail_source_key"),
            ordinal: column(row, prefix, "tail_ordinal"),
            slice_id: column(row, prefix, "tail_slice_id"),
        });
        Self {
            system_id: column(row, prefix, "system_id"),
            military_points: column(row, prefix, "military_points"),
            industrial_points: column(row, prefix, "industrial_points"),
            evaluated_at: column(row, prefix, "evaluated_at"),
            revision: column(row, prefix, "revision"),
            activation_ordinal: column(row, prefix, "activation_ordinal"),
            trend_start_at: column(row, prefix, "trend_start_at"),
            military_trend_base: column(row, prefix, "military_trend_base"),
            industrial_trend_base: column(row, prefix, "industrial_trend_base"),
            tail,
        }
    }
    fn from_row(row: &Row) -> Self {
        Self::from_columns(row, "")
    }
}

/// Folded points baseline at a checkpoint; facts strictly before `checkpoint_at` are already included.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CheckpointRow {
    pub system_id: i64,
    pub checkpoint_at: i64,
    pub military_points: f64,
    pub industrial_points: f64,
    pub activation_ordinal: i64,
}
impl CheckpointRow {
    fn from_row(row: &Row) -> Self {
        Self { system_id: row.get("system_id"), checkpoint_at: row.get("checkpoint_at"), military_points: row.get("military_points"), industrial_points: row.get("industrial_points"), activation_ordinal: row.get("activation_ordinal") }
    }
}

/// One requested system's joined rows; every member is absent for a known system with no activity.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservationRow {
    pub system_id: i64,
    pub system: Option<SystemRow>,
    pub development: Option<DevelopmentRow>,
    pub capital: Option<PolicyRow>,
}

/// Membership check against the caller's captured activation history: the row
/// with this ordinal must exist and belong to the development-index namespace.
/// The captured history is ordinal-ascending, so this is a binary search.
fn activation_known(history: &[ActivationRow], activation: i64) -> bool {
    history.binary_search_by_key(&activation, |row| row.ordinal)
        .is_ok_and(|index| history[index].namespace == NAMESPACE)
}

fn unknown_activation() -> db::Error {
    db::Error::Progress(anyhow::anyhow!("unknown sovereignty indices activation ordinal"))
}

/// Insert the initial zero row when absent; races resolve to the same stored row.
/// Runs outside the delivery transaction so two first receipts never deadlock.
/// `history` is the caller's captured activation history for this namespace.
pub(crate) async fn ensure(ex: &(impl Executor + ?Sized), system: i64, initial_at: i64, activation: i64, history: &[ActivationRow]) -> Result<(), db::Error> {
    if system <= 0 || initial_at <= 0 || activation <= 0 {
        return Err(db::Error::Progress(anyhow::anyhow!("invalid development ensure arguments")));
    }
    if !activation_known(history, activation) {
        return Err(unknown_activation());
    }
    ex.execute(
        "INSERT INTO game.sov_development (system_id, military_points, industrial_points, evaluated_at, revision, activation_ordinal, trend_start_at, military_trend_base, industrial_trend_base) \
         VALUES ($1, 0.0, 0.0, $2, 0, $3, $2, 0.0, 0.0) ON CONFLICT (system_id) DO NOTHING",
        &[&system, &initial_at, &activation],
    ).await?;
    Ok(())
}

pub(crate) async fn read(ex: &(impl Executor + ?Sized), system: i64) -> Result<Option<DevelopmentRow>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLUMNS} FROM game.sov_development WHERE system_id = $1"), &[&system]).await?.map(|row| DevelopmentRow::from_row(&row)))
}

/// Row lock only; the caller must hold a transaction and `ensure` any absent row beforehand.
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), system: i64) -> Result<Option<DevelopmentRow>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLUMNS} FROM game.sov_development WHERE system_id = $1 FOR UPDATE"), &[&system]).await?.map(|row| DevelopmentRow::from_row(&row)))
}

/// One joined snapshot per requested system ID, driven by the input IDs.
pub(crate) async fn observations(ex: &(impl Executor + ?Sized), systems: &[i64]) -> Result<Vec<ObservationRow>, db::Error> {
    let rows = ex.query(
        "SELECT input.system_id AS system_id, \
            s.system_id AS s_system_id, s.hub_item_id AS s_hub_item_id, s.claim_corporation_id AS s_claim_corporation_id, s.alliance_id AS s_alliance_id, s.claim_time AS s_claim_time, s.ownership_epoch AS s_ownership_epoch, s.revision AS s_revision, \
            d.system_id AS d_system_id, d.military_points AS d_military_points, d.industrial_points AS d_industrial_points, d.evaluated_at AS d_evaluated_at, d.revision AS d_revision, d.activation_ordinal AS d_activation_ordinal, d.trend_start_at AS d_trend_start_at, d.military_trend_base AS d_military_trend_base, d.industrial_trend_base AS d_industrial_trend_base, d.tail_occurred_at AS d_tail_occurred_at, d.tail_namespace AS d_tail_namespace, d.tail_source_key AS d_tail_source_key, d.tail_ordinal AS d_tail_ordinal, d.tail_slice_id AS d_tail_slice_id, \
            p.alliance_id AS p_alliance_id, p.current_capital_system AS p_current_capital_system, p.last_capital_changed_at AS p_last_capital_changed_at, p.capital_lost_at AS p_capital_lost_at, p.capital_change_not_before AS p_capital_change_not_before, p.revision AS p_revision \
         FROM unnest($1::bigint[]) AS input(system_id) \
         LEFT JOIN game.sov_systems s ON s.system_id = input.system_id \
         LEFT JOIN game.sov_development d ON d.system_id = input.system_id \
         LEFT JOIN game.sov_alliance_policy p ON p.alliance_id = s.alliance_id \
         ORDER BY input.system_id",
        &[&systems],
    ).await?;
    Ok(rows.iter().map(|row| {
        let system = row.get::<_, Option<i64>>("s_system_id").map(|system_id| SystemRow {
            system_id,
            hub_item_id: row.get("s_hub_item_id"),
            claim_corporation_id: row.get("s_claim_corporation_id"),
            alliance_id: row.get("s_alliance_id"),
            claim_time: row.get("s_claim_time"),
            ownership_epoch: row.get("s_ownership_epoch"),
            revision: row.get("s_revision"),
        });
        let development = row.get::<_, Option<i64>>("d_system_id").map(|_| DevelopmentRow::from_columns(row, "d_"));
        let capital = row.get::<_, Option<i64>>("p_alliance_id").map(|alliance_id| PolicyRow {
            alliance_id,
            current_capital_system: row.get("p_current_capital_system"),
            last_capital_changed_at: row.get("p_last_capital_changed_at"),
            capital_lost_at: row.get("p_capital_lost_at"),
            capital_change_not_before: row.get("p_capital_change_not_before"),
            revision: row.get("p_revision"),
        });
        ObservationRow { system_id: row.get("system_id"), system, development, capital }
    }).collect())
}

/// Guarded replace: one affected row only when the stored revision still matches.
/// A false result is a stale prepare, never a partial write.
/// `history` is the caller's captured activation history for this namespace.
pub(crate) async fn replace(ex: &(impl Executor + ?Sized), expected_revision: i64, next: &DevelopmentRow, history: &[ActivationRow]) -> Result<bool, db::Error> {
    if !activation_known(history, next.activation_ordinal) {
        return Err(unknown_activation());
    }
    let (tail_at, tail_namespace, tail_key, tail_ordinal, tail_slice) = match &next.tail {
        Some(tail) => (Some(tail.occurred_at), Some(tail.source_namespace.as_str()), Some(tail.source_key.as_str()), Some(tail.ordinal), Some(tail.slice_id)),
        None => (None, None, None, None, None),
    };
    let changed = ex.execute(
        "UPDATE game.sov_development SET military_points = $3, industrial_points = $4, evaluated_at = $5, revision = revision + 1, activation_ordinal = $6, trend_start_at = $7, military_trend_base = $8, industrial_trend_base = $9, tail_occurred_at = $10, tail_namespace = $11, tail_source_key = $12, tail_ordinal = $13, tail_slice_id = $14 \
         WHERE system_id = $1 AND revision = $2",
        &[&next.system_id, &expected_revision, &next.military_points, &next.industrial_points, &next.evaluated_at, &next.activation_ordinal, &next.trend_start_at, &next.military_trend_base, &next.industrial_trend_base, &tail_at, &tail_namespace, &tail_key, &tail_ordinal, &tail_slice],
    ).await?;
    Ok(changed == 1)
}

/// Latest checkpoint at or before `at`; its points cover all facts strictly before `checkpoint_at`.
pub(crate) async fn checkpoint_before(ex: &(impl Executor + ?Sized), system: i64, at: i64) -> Result<Option<CheckpointRow>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {CHECKPOINT_COLUMNS} FROM game.sov_development_checkpoints WHERE system_id = $1 AND checkpoint_at <= $2 ORDER BY checkpoint_at DESC LIMIT 1"), &[&system, &at]).await?.map(|row| CheckpointRow::from_row(&row)))
}

/// Replace the checkpoint suffix at and after `from` with the supplied rows;
/// the caller runs this in the same transaction as the state `replace`.
/// `history` is the caller's captured activation history for this namespace;
/// every checkpoint's system and activation membership is validated before any
/// row is deleted or inserted.
pub(crate) async fn replace_checkpoints(ex: &(impl Executor + ?Sized), system: i64, from: i64, checkpoints: &[CheckpointRow], history: &[ActivationRow]) -> Result<(), db::Error> {
    if checkpoints.iter().any(|row| row.system_id != system) {
        return Err(db::Error::Progress(anyhow::anyhow!("checkpoint system mismatch")));
    }
    if checkpoints.iter().any(|row| !activation_known(history, row.activation_ordinal)) {
        return Err(unknown_activation());
    }
    ex.execute("DELETE FROM game.sov_development_checkpoints WHERE system_id = $1 AND checkpoint_at >= $2", &[&system, &from]).await?;
    if checkpoints.is_empty() {
        return Ok(());
    }
    let at: Vec<i64> = checkpoints.iter().map(|row| row.checkpoint_at).collect();
    let military: Vec<f64> = checkpoints.iter().map(|row| row.military_points).collect();
    let industrial: Vec<f64> = checkpoints.iter().map(|row| row.industrial_points).collect();
    let activation: Vec<i64> = checkpoints.iter().map(|row| row.activation_ordinal).collect();
    ex.execute(
        "INSERT INTO game.sov_development_checkpoints (system_id, checkpoint_at, military_points, industrial_points, activation_ordinal) \
         SELECT $1, c.checkpoint_at, c.military_points, c.industrial_points, c.activation_ordinal \
         FROM unnest($2::bigint[], $3::float8[], $4::float8[], $5::bigint[]) AS c(checkpoint_at, military_points, industrial_points, activation_ordinal)",
        &[&system, &at, &military, &industrial, &activation],
    ).await?;
    Ok(())
}

/// Keyset page over systems needing index maintenance: still-decaying points or
/// trend baselines, union every system holding a valid claim.
pub(crate) async fn active_systems(ex: &(impl Executor + ?Sized), after: i64, limit: i64) -> Result<Vec<i64>, db::Error> {
    if limit <= 0 {
        return Err(db::Error::Progress(anyhow::anyhow!("invalid active systems limit")));
    }
    Ok(ex.query(
        "SELECT system_id FROM ( \
            SELECT system_id FROM game.sov_development \
            WHERE military_points > 0 OR industrial_points > 0 OR military_trend_base > 0 OR industrial_trend_base > 0 \
            UNION \
            SELECT system_id FROM game.sov_systems WHERE claim_time IS NOT NULL \
         ) active WHERE system_id > $1 ORDER BY system_id LIMIT $2",
        &[&after, &limit],
    ).await?.iter().map(|row| row.get("system_id")).collect())
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
