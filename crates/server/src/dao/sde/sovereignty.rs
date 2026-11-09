//! Targeted startup read of `sde_native.spacecomponentsbytype` for sovereignty.
//! Client deploy and entosis component consumers read the corresponding definitions;
//! validation is performed by the immutable SDE projection, not this DAO.
//! Other component types and runtime state are not loaded here.
use crate::db::{self, executor::Executor};

pub(crate) struct ComponentRow { pub(crate) key: Option<String>, pub(crate) data: String }

impl ComponentRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { key: row.get("key"), data: row.get("data") }
    }
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized), type_id: i32) -> Result<Vec<ComponentRow>, db::Error> {
    let key = type_id.to_string();
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.spacecomponentsbytype WHERE key = $1", &[&key]).await?;
    Ok(rows.iter().map(ComponentRow::from_row).collect())
}
