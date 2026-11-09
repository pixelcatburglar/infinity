//! Startup read of native contribution-method metadata.
//! Client goal configuration consumes these definitions; runtime progress is not handled here.
use crate::db::{self, executor::Executor};

pub(crate) struct Row { pub(crate) key: Option<String>, pub(crate) data: String }

impl Row {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { key: row.get("key"), data: row.get("data") }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<Row>, db::Error> {
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.contributionmethods", &[]).await?;
    Ok(rows.iter().map(Row::from_row).collect())
}
