//! Startup read of native command-burst modifier collections.
//! Runtime buff state consumes these definitions through `sde::dbuff`; no
//! runtime or UI fields are projected here.
use crate::db::{self, executor::Executor};

pub(crate) struct Row { pub(crate) key: Option<String>, pub(crate) data: String }

impl Row {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { key: row.get("key"), data: row.get("data") }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<Row>, db::Error> {
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.dbuffcollections ORDER BY key", &[]).await?;
    Ok(rows.iter().map(Row::from_row).collect())
}
