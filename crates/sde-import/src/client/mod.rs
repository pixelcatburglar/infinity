//! Client static-data preparation and shared orchestration types.
pub mod blueprints;
pub mod catalogues;
pub mod cfsd;
pub mod dogma;
pub mod families;
pub mod input;
pub mod localization;
pub mod materials;
pub mod normalize;
pub mod ownership;
pub mod pickle;
pub mod prepare;
pub mod readback;
pub mod types;
pub mod validate;

use std::collections::BTreeMap;
use anyhow::Result;
use serde_json::Value;

pub type FamilyRows = BTreeMap<String, Vec<Value>>;

pub struct Decoded {
    pub native: FamilyRows,
    pub native_raw: BTreeMap<String, Vec<String>>,
    pub compat_raw: BTreeMap<String, Vec<String>>,
}

pub struct Prepared {
    pub decoded: Decoded,
    pub inputs: Vec<input::Input>,
    pub jobs: Vec<crate::Job>,
    pub typed_sql: String,
    pub metadata: BTreeMap<String, Value>,
    typed_expected: readback::TypedExpected,
}

pub async fn preflight_database(client: &mut tokio_postgres::Client, decoded: &Decoded) -> Result<validate::DbPreflight> {
    validate::preflight_database(client, decoded).await
}

pub async fn snapshot_unmigrated(client: &tokio_postgres::Client) -> Result<readback::UnmigratedSnapshot> {
    readback::snapshot_unmigrated(client).await
}

pub async fn readback_database(client: &tokio_postgres::Client, prepared: Prepared, baseline: &readback::UnmigratedSnapshot) -> Result<readback::Readback> {
    readback::readback(client, prepared, baseline).await
}
