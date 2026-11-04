//! Native expert-system rows loaded once at startup from `sde_native.expertsystems`.
//! The client source is `expertSystems/client/service.py` (`GetMyExpertSystems` and
//! lifecycle notifications); imported native data is authoritative because an exact
//! fsdbinary comparison with the client resources is unavailable here. This DAO
//! only reads named columns and leaves validation and type references to the SDE
//! projection.
use crate::db::{self, executor::Executor};

pub(crate) struct ExpertSystemRow {
    pub(crate) key: Option<String>,
    pub(crate) data: String,
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<ExpertSystemRow>, db::Error> {
    let rows = ex.query(
        "SELECT key, data::text AS data FROM sde_native.expertsystems ORDER BY row_no",
        &[],
    ).await?;
    Ok(rows.iter().map(|row| ExpertSystemRow {
        key: row.get("key"),
        data: row.get("data"),
    }).collect())
}
