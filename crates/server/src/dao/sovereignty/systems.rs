//! System ownership rows in game.sov_systems; lifecycle rules and transactions are caller-owned.
use crate::db::{self, executor::Executor};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SystemRow {
    pub system_id: i64, pub hub_item_id: Option<i64>, pub claim_corporation_id: Option<i64>,
    pub alliance_id: Option<i64>, pub claim_time: Option<i64>, pub ownership_epoch: i64, pub revision: i64,
}
impl SystemRow {
    pub(super) fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { system_id: row.get("system_id"), hub_item_id: row.get("hub_item_id"), claim_corporation_id: row.get("claim_corporation_id"), alliance_id: row.get("alliance_id"), claim_time: row.get("claim_time"), ownership_epoch: row.get("ownership_epoch"), revision: row.get("revision") }
    }
}
pub(crate) struct SystemChange {
    pub hub_item_id: Option<i64>, pub claim_corporation_id: Option<i64>, pub alliance_id: Option<i64>, pub claim_time: Option<i64>, pub ownership_epoch: i64,
}
const COLUMNS: &str = "system_id, hub_item_id, claim_corporation_id, alliance_id, claim_time, ownership_epoch, revision";
pub(crate) async fn read(ex: &(impl Executor + ?Sized), system: i64) -> Result<Option<SystemRow>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLUMNS} FROM game.sov_systems WHERE system_id=$1"), &[&system]).await?.map(|r| SystemRow::from_row(&r)))
}
/// The caller must hold a transaction for the returned row lock.
pub(crate) async fn lock(ex: &(impl Executor + ?Sized), system: i64) -> Result<SystemRow, db::Error> {
    ex.execute("INSERT INTO game.sov_systems (system_id) VALUES ($1) ON CONFLICT DO NOTHING", &[&system]).await?;
    Ok(SystemRow::from_row(&ex.query_one(&format!("SELECT {COLUMNS} FROM game.sov_systems WHERE system_id=$1 FOR UPDATE"), &[&system]).await?))
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized)) -> Result<Vec<SystemRow>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.sov_systems ORDER BY system_id"), &[]).await?.iter().map(SystemRow::from_row).collect())
}
/// Read only the requested static-map system IDs in one statement.
pub(crate) async fn for_systems(ex: &(impl Executor + ?Sized), systems: &[i64]) -> Result<Vec<SystemRow>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.sov_systems WHERE system_id=ANY($1) ORDER BY system_id"), &[&systems]).await?.iter().map(SystemRow::from_row).collect())
}
pub(crate) async fn for_alliance(ex: &(impl Executor + ?Sized), alliance: i64) -> Result<Vec<SystemRow>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.sov_systems WHERE alliance_id=$1 ORDER BY system_id"), &[&alliance]).await?.iter().map(SystemRow::from_row).collect())
}
/// Return the complete ordered set of systems currently claimed by a corporation.
pub(crate) async fn for_corporation(ex: &(impl Executor + ?Sized), corporation: i64) -> Result<Vec<SystemRow>, db::Error> {
    Ok(ex.query(&format!("SELECT {COLUMNS} FROM game.sov_systems WHERE claim_corporation_id=$1 ORDER BY system_id"), &[&corporation]).await?.iter().map(SystemRow::from_row).collect())
}
pub(crate) async fn replace(ex: &(impl Executor + ?Sized), expected: &SystemRow, change: &SystemChange) -> Result<Option<SystemRow>, db::Error> {
    Ok(ex.query_opt(&format!("UPDATE game.sov_systems SET hub_item_id=$4, claim_corporation_id=$5, alliance_id=$6, claim_time=$7, ownership_epoch=$8, revision=revision+1 WHERE system_id=$1 AND hub_item_id IS NOT DISTINCT FROM $2 AND ownership_epoch=$3 AND revision=$9 AND $8 >= ownership_epoch AND $8::bigint::numeric - ownership_epoch::numeric <= 1 RETURNING {COLUMNS}"), &[&expected.system_id, &expected.hub_item_id, &expected.ownership_epoch, &change.hub_item_id, &change.claim_corporation_id, &change.alliance_id, &change.claim_time, &change.ownership_epoch, &expected.revision]).await?.map(|r| SystemRow::from_row(&r)))
}
