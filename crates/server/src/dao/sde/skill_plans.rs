//! Native certified skill plan rows loaded once at startup from `sde_native.skillplans`.
//! The projection is used for certified tracking; ownership and wire responses are elsewhere.
//! This module does not issue runtime static-data queries.
use crate::db::{self, executor::Executor};

pub(crate) struct SkillPlanRow { pub key: Option<String>, pub data: String }
pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<SkillPlanRow>, db::Error> {
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.skillplans ORDER BY row_no", &[]).await?;
    Ok(rows.iter().map(|row| SkillPlanRow { key: row.get("key"), data: row.get("data") }).collect())
}

pub(crate) async fn requirements(ex: &(impl Executor + ?Sized)) -> Result<Vec<SkillPlanRow>, db::Error> {
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.requiredskillsfortypes ORDER BY row_no", &[]).await?;
    Ok(rows.iter().map(|row| SkillPlanRow { key: row.get("key"), data: row.get("data") }).collect())
}
