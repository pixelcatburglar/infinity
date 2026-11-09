//! Typed persistent adaptation chip balances and their retained content version.
//! The client-facing ship adaptation service owns transactions and policy; node,
//! mission and effect state is not stored here.

pub(crate) mod chips;

use crate::db::{self, executor::Executor};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Subject {
    pub revision: i64,
    pub content_namespace: String,
    pub content_version: String,
}

impl Subject {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { revision: row.get("revision"), content_namespace: row.get("content_namespace"), content_version: row.get("content_version") }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub subject: Option<Subject>,
    pub chips: Vec<chips::Chip>,
}

impl Snapshot {
    pub(crate) fn revision(&self) -> i64 { self.subject.as_ref().map_or(0, |subject| subject.revision) }
}

/// One statement observes the subject and all balances under one MVCC snapshot.
pub(crate) async fn snapshot(ex: &(impl Executor + ?Sized), character: i64) -> Result<Snapshot, db::Error> {
    let rows = ex.query("SELECT s.revision, s.content_namespace, s.content_version, c.type_id, c.balance FROM (SELECT $1::bigint AS character_id) requested LEFT JOIN game.ship_adaption_characters s ON s.character_id = requested.character_id LEFT JOIN game.ship_adaption_chips c ON c.character_id = s.character_id ORDER BY c.type_id", &[&character]).await?;
    let subject = rows.first().and_then(|row| row.get::<_, Option<i64>>("revision").map(|_| Subject::from_row(row)));
    let chips = rows.iter().filter_map(|row| row.get::<_, Option<i32>>("type_id").map(|_| chips::Chip::from_row(row))).collect();
    Ok(Snapshot { subject, chips })
}

/// Call inside the caller's transaction, after locking character authority.
pub(crate) async fn lock_subject(ex: &(impl Executor + ?Sized), character: i64, version: &str) -> Result<Subject, db::Error> {
    ex.execute("INSERT INTO game.ship_adaption_characters (character_id, content_namespace, content_version) VALUES ($1, 'ship_adaption', $2) ON CONFLICT (character_id) DO NOTHING", &[&character, &version]).await?;
    let row = ex.query_one("SELECT revision, content_namespace, content_version FROM game.ship_adaption_characters WHERE character_id = $1 FOR UPDATE", &[&character]).await?;
    Ok(Subject::from_row(&row))
}

/// The caller must reject a stale expected revision and roll back on a miss.
pub(crate) async fn advance(ex: &(impl Executor + ?Sized), character: i64, expected: i64, version: &str) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.ship_adaption_characters SET revision = revision + 1, content_version = $3 WHERE character_id = $1 AND revision = $2 AND revision < 9223372036854775807", &[&character, &expected, &version]).await? == 1)
}

#[cfg(test)]
mod tests;
