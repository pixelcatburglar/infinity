//! Persistent typed Lua values in `game.lua_store`; no client module uses this
//! table directly. Script names are bound by the host, not supplied by scripts.
//! JSON encoding and size limits are host policy.

use crate::db::{self, executor::Executor};

pub(crate) async fn get(ex: &(impl Executor + ?Sized), script: &str, key: &str) -> Result<Option<String>, db::Error> {
    Ok(ex.query_opt("SELECT value::text AS value FROM game.lua_store WHERE script = $1 AND key = $2", &[&script, &key]).await?.map(|row| row.get("value")))
}

pub(crate) struct StoreRow {
    pub key: String,
    pub value: String,
}

impl StoreRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { key: row.get("key"), value: row.get("value") }
    }
}

pub(crate) async fn get_many(ex: &(impl Executor + ?Sized), script: &str, keys: &[String]) -> Result<Vec<StoreRow>, db::Error> {
    Ok(ex.query("SELECT key, value::text AS value FROM game.lua_store WHERE script = $1 AND key = ANY($2)", &[&script, &keys]).await?
        .into_iter().map(|row| StoreRow::from_row(&row)).collect())
}

pub(crate) async fn set(ex: &(impl Executor + ?Sized), script: &str, key: &str, value: &str) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.lua_store (script, key, value) VALUES ($1, $2, $3::text::jsonb) ON CONFLICT (script, key) DO UPDATE SET value = EXCLUDED.value", &[&script, &key, &value]).await?;
    Ok(())
}

pub(crate) async fn delete(ex: &(impl Executor + ?Sized), script: &str, key: &str) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.lua_store WHERE script = $1 AND key = $2", &[&script, &key]).await? != 0)
}

pub(crate) async fn keys(ex: &(impl Executor + ?Sized), script: &str, prefix: &str) -> Result<Vec<String>, db::Error> {
    Ok(ex.query("SELECT key FROM game.lua_store WHERE script = $1 AND left(key, length($2)) = $2 ORDER BY key", &[&script, &prefix]).await?.into_iter().map(|row| row.get("key")).collect())
}

#[cfg(test)]
mod tests;
