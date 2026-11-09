//! Active character membership and organization from `game.characters`, corporations
//! and alliances, excluding deleted characters and deleted game organizations.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Membership policy, transactions, refusals, notifications and World are caller concerns.

use crate::db::{executor::Executor, Error};

struct Member { character_id: i64 }

impl Member {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { character_id: row.get("character_id") }
    }
}

pub(crate) async fn active_ids(ex: &(impl Executor + ?Sized), corp_id: i64) -> Result<Vec<i64>, Error> {
    Ok(ex.query("SELECT character_id FROM game.characters WHERE corporation_id = $1 AND NOT deleted", &[&corp_id])
        .await?.iter().map(|row| Member::from_row(row).character_id).collect())
}

pub(crate) struct Organization {
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
}

impl Organization {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { corporation_id: row.get("corporation_id"), alliance_id: row.get("alliance_id") }
    }
}

pub(crate) async fn organization(ex: &(impl Executor + ?Sized), character_id: i64) -> Result<Option<Organization>, Error> {
    Ok(ex.query_opt("SELECT ch.corporation_id, a.alliance_id FROM game.characters ch LEFT JOIN game.corporations co ON co.corporation_id = ch.corporation_id LEFT JOIN game.alliances a ON a.alliance_id = co.alliance_id AND NOT a.deleted WHERE ch.character_id = $1 AND NOT ch.deleted AND (co.corporation_id IS NULL OR NOT co.deleted)", &[&character_id])
        .await?.as_ref().map(Organization::from_row))
}

/// Current corporation IDs for active characters, with the same deletion rules as `organization`.
pub(crate) async fn organizations(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<std::collections::HashMap<i64, i64>, Error> {
    let rows = ex.query("SELECT ch.character_id, ch.corporation_id FROM game.characters ch LEFT JOIN game.corporations co ON co.corporation_id = ch.corporation_id WHERE ch.character_id = ANY($1) AND NOT ch.deleted AND (co.corporation_id IS NULL OR NOT co.deleted)", &[&ids]).await?;
    Ok(rows.iter().map(|row| (row.get("character_id"), row.get("corporation_id"))).collect())
}
