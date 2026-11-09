//! Hub structure rows and item-backed snapshots; lifecycle authorization is caller-owned.
use crate::db::{self, executor::Executor};
use super::systems::SystemRow;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StructureRow {
    pub item_id: i64, pub system_id: i64, pub source_item_id: i64,
    pub deployer_character_id: i64, pub deployer_corporation_id: i64, pub deployed_at: i64,
    pub content_namespace: String, pub content_version: String,
    pub self_destruct_due_at: Option<i64>, pub self_destruct_actor: Option<i64>,
}
impl StructureRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { item_id:r.get("item_id"), system_id:r.get("system_id"), source_item_id:r.get("source_item_id"), deployer_character_id:r.get("deployer_character_id"), deployer_corporation_id:r.get("deployer_corporation_id"), deployed_at:r.get("deployed_at"), content_namespace:r.get("content_namespace"), content_version:r.get("content_version"), self_destruct_due_at:r.get("self_destruct_due_at"), self_destruct_actor:r.get("self_destruct_actor") }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HubRow {
    pub system: SystemRow, pub structure: StructureRow, pub type_id: i32, pub owner_id: i64,
    pub x: Option<f64>, pub y: Option<f64>, pub z: Option<f64>,
}
impl HubRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { system: SystemRow::from_row(r), structure: StructureRow::from_row(r), type_id:r.get("type_id"), owner_id:r.get("owner_id"), x:r.get("x"), y:r.get("y"), z:r.get("z") }
    }
}
const COLS: &str = "item_id, system_id, source_item_id, deployer_character_id, deployer_corporation_id, deployed_at, content_namespace, content_version, self_destruct_due_at, self_destruct_actor";
pub(crate) async fn read(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<StructureRow>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLS} FROM game.sov_structures WHERE item_id=$1"), &[&item]).await?.map(|r| StructureRow::from_row(&r)))
}
pub(crate) async fn for_system(ex: &(impl Executor + ?Sized), system: i64) -> Result<Option<StructureRow>, db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLS} FROM game.sov_structures WHERE system_id=$1"), &[&system]).await?.map(|r| StructureRow::from_row(&r)))
}
pub(crate) async fn insert(ex: &(impl Executor + ?Sized), row: &StructureRow) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.sov_structures (item_id,system_id,source_item_id,deployer_character_id,deployer_corporation_id,deployed_at,content_namespace,content_version,self_destruct_due_at,self_destruct_actor) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT DO NOTHING", &[&row.item_id,&row.system_id,&row.source_item_id,&row.deployer_character_id,&row.deployer_corporation_id,&row.deployed_at,&row.content_namespace,&row.content_version,&row.self_destruct_due_at,&row.self_destruct_actor]).await? == 1)
}
pub(crate) async fn set_destruct(ex: &(impl Executor + ?Sized), item: i64, expected_due: Option<i64>, actor: Option<i64>, due: Option<i64>) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.sov_structures SET self_destruct_actor=$3,self_destruct_due_at=$4 WHERE item_id=$1 AND self_destruct_due_at IS NOT DISTINCT FROM $2", &[&item,&expected_due,&actor,&due]).await? == 1)
}
pub(crate) async fn remove(ex: &(impl Executor + ?Sized), item: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.sov_structures WHERE item_id=$1", &[&item]).await? == 1)
}
pub(crate) async fn snapshots(ex: &(impl Executor + ?Sized), system: Option<i64>, alliance: Option<i64>) -> Result<Vec<HubRow>, db::Error> {
    Ok(ex.query("SELECT s.system_id,s.hub_item_id,s.claim_corporation_id,s.alliance_id,s.claim_time,s.ownership_epoch,s.revision,h.item_id,h.source_item_id,h.deployer_character_id,h.deployer_corporation_id,h.deployed_at,h.content_namespace,h.content_version,h.self_destruct_due_at,h.self_destruct_actor,i.type_id,i.owner_id,i.x,i.y,i.z FROM game.sov_systems s JOIN game.sov_structures h ON h.system_id=s.system_id AND h.item_id=s.hub_item_id JOIN game.items i ON i.item_id=h.item_id WHERE ($1::bigint IS NULL OR s.system_id=$1) AND ($2::bigint IS NULL OR s.alliance_id=$2) ORDER BY s.system_id,h.item_id", &[&system,&alliance]).await?.iter().map(HubRow::from_row).collect())
}
