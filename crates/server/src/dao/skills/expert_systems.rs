//! Persistent expert-system installation lifecycles in `game.character_expert_systems`.
//! The table stores only character/type and installation timestamps; static definitions,
//! slot policy, expiry reconciliation, transactions, notifications and World belong to
//! the owning services. There is no acquisition or client protocol logic here.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

/// One persisted expert-system installation and its half-open active interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExpertSystem {
    pub character_id: i64,
    pub type_id: i32,
    pub installed_at: i64,
    pub expires_at: i64,
}

impl ExpertSystem {
    fn from_row(row: &Row) -> Self {
        Self {
            character_id: row.get("character_id"),
            type_id: row.get("type_id"),
            installed_at: row.get("installed_at"),
            expires_at: row.get("expires_at"),
        }
    }
}

/// Read every persisted installation for a character, including expired rows.
pub(crate) async fn all(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
) -> Result<Vec<ExpertSystem>, db::Error> {
    Ok(ex
        .query(
            "SELECT character_id, type_id, installed_at, expires_at
             FROM game.character_expert_systems
             WHERE character_id = $1 ORDER BY type_id",
            &[&character_id],
        )
        .await?
        .iter()
        .map(ExpertSystem::from_row)
        .collect())
}

/// Read installations active at `at`; the upper bound is deliberately exclusive.
pub(crate) async fn active(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    at: i64,
) -> Result<Vec<ExpertSystem>, db::Error> {
    Ok(ex
        .query(
            "SELECT character_id, type_id, installed_at, expires_at
             FROM game.character_expert_systems
             WHERE character_id = $1 AND installed_at <= $2 AND expires_at > $2
             ORDER BY type_id",
            &[&character_id, &at],
        )
        .await?
        .iter()
        .map(ExpertSystem::from_row)
        .collect())
}
pub(crate) async fn active_many(
    ex: &(impl Executor + ?Sized),
    character_ids: &[i64],
    at: i64,
) -> Result<Vec<ExpertSystem>, db::Error> {
    Ok(ex.query(
        "SELECT character_id, type_id, installed_at, expires_at
         FROM game.character_expert_systems
         WHERE character_id = ANY($1) AND installed_at <= $2 AND expires_at > $2
         ORDER BY character_id, type_id",
        &[&character_ids, &at],
    ).await?.iter().map(ExpertSystem::from_row).collect())
}


/// Return characters with an installation expiring at or before `at`.
pub(crate) async fn timed_character_ids(
    ex: &(impl Executor + ?Sized),
    at: i64,
) -> Result<Vec<i64>, db::Error> {
    Ok(ex
        .query(
            "SELECT DISTINCT character_id
             FROM game.character_expert_systems
             WHERE expires_at <= $1 ORDER BY character_id",
            &[&at],
        )
        .await?
        .iter()
        .map(|row| row.get("character_id"))
        .collect())
}

/// Read a character's rows while holding them locked for the caller's transaction.
pub(crate) async fn lock_rows(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
) -> Result<Vec<ExpertSystem>, db::Error> {
    Ok(ex
        .query(
            "SELECT character_id, type_id, installed_at, expires_at
             FROM game.character_expert_systems
             WHERE character_id = $1 ORDER BY type_id FOR UPDATE",
            &[&character_id],
        )
        .await?
        .iter()
        .map(ExpertSystem::from_row)
        .collect())
}

/// Insert a new installation only when that type is not already present.
pub(crate) async fn insert(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    type_id: i32,
    installed_at: i64,
    expires_at: i64,
) -> Result<bool, db::Error> {
    Ok(ex
        .execute(
            "INSERT INTO game.character_expert_systems
                 (character_id, type_id, installed_at, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (character_id, type_id) DO NOTHING",
            &[&character_id, &type_id, &installed_at, &expires_at],
        )
        .await?
        == 1)
}

/// Extend a row only if its expiry is the value observed by the caller.
pub(crate) async fn renew(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    type_id: i32,
    observed_expires_at: i64,
    expires_at: i64,
) -> Result<bool, db::Error> {
    Ok(ex
        .execute(
            "UPDATE game.character_expert_systems
             SET expires_at = $4
             WHERE character_id = $1 AND type_id = $2
               AND expires_at = $3 AND $4 > expires_at",
            &[&character_id, &type_id, &observed_expires_at, &expires_at],
        )
        .await?
        == 1)
}

/// Delete one installation only when both its type and observed expiry match.
pub(crate) async fn delete(
    ex: &(impl Executor + ?Sized),
    character_id: i64,
    type_id: i32,
    observed_expires_at: i64,
) -> Result<bool, db::Error> {
    Ok(ex
        .execute(
            "DELETE FROM game.character_expert_systems
             WHERE character_id = $1 AND type_id = $2 AND expires_at = $3",
            &[&character_id, &type_id, &observed_expires_at],
        )
        .await?
        == 1)
}
