//! Startup-only access to native crate JSONB rows.
//! The native table is retained as provenance; policy content is validated by the SDE catalogue.
use crate::db::executor::Executor;
use serde_json::Value;

pub(crate) struct NativeRow { pub key: String, pub data: Value }

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> anyhow::Result<Vec<NativeRow>> {
    let rows = ex.query("SELECT key, data::text AS data FROM sde_native.crates ORDER BY key", &[]).await?;
    rows.into_iter().map(|row| {
        let text: String = row.get("data");
        let data = serde_json::from_str(&text)?;
        Ok(NativeRow { key: row.get("key"), data })
    }).collect()
}
