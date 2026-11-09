//! Capture checkpoints in game.sov_capture_targets; no offline interpolation or link persistence.
use crate::db::{self, executor::Executor};
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CaptureRow {
    pub target_item_id:i64, pub system_id:i64, pub ownership_epoch:i64,
    pub controlling_team:Option<i64>, pub scoring_team:Option<i64>, pub defender_team:i64,
    pub score:f64, pub rate:f64, pub blocked:bool, pub score_sim_time:i64, pub checkpoint_wall_time:i64,
    pub revision:i64, pub capture_time_seconds:f64, pub defensive_regen_rate:f64,
    pub checkpoint_interval_ms:i64, pub content_namespace:String, pub content_version:String,
}
impl CaptureRow {
    fn from_row(r:&tokio_postgres::Row)->Self { Self { target_item_id:r.get("target_item_id"), system_id:r.get("system_id"), ownership_epoch:r.get("ownership_epoch"), controlling_team:r.get("controlling_team"), scoring_team:r.get("scoring_team"), defender_team:r.get("defender_team"), score:r.get("score"), rate:r.get("rate"), blocked:r.get("blocked"), score_sim_time:r.get("score_sim_time"), checkpoint_wall_time:r.get("checkpoint_wall_time"), revision:r.get("revision"), capture_time_seconds:r.get("capture_time_seconds"), defensive_regen_rate:r.get("defensive_regen_rate"), checkpoint_interval_ms:r.get("checkpoint_interval_ms"), content_namespace:r.get("content_namespace"), content_version:r.get("content_version") } }
}
const COLS:&str="target_item_id,system_id,ownership_epoch,controlling_team,scoring_team,defender_team,score,rate,blocked,score_sim_time,checkpoint_wall_time,revision,capture_time_seconds,defensive_regen_rate,checkpoint_interval_ms,content_namespace,content_version";
pub(crate) async fn read(ex:&(impl Executor+?Sized), target:i64)->Result<Option<CaptureRow>,db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLS} FROM game.sov_capture_targets WHERE target_item_id=$1"),&[&target]).await?.map(|r|CaptureRow::from_row(&r)))
}
pub(crate) async fn for_system(ex:&(impl Executor+?Sized), system:i64)->Result<Option<CaptureRow>,db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLS} FROM game.sov_capture_targets WHERE system_id=$1"),&[&system]).await?.map(|r|CaptureRow::from_row(&r)))
}
/// Lock the current capture anchor until the transaction ends.
pub(crate) async fn lock_for_system(ex:&(impl Executor+?Sized), system:i64)->Result<Option<CaptureRow>,db::Error> {
    Ok(ex.query_opt(&format!("SELECT {COLS} FROM game.sov_capture_targets WHERE system_id=$1 FOR UPDATE"),&[&system]).await?.map(|r|CaptureRow::from_row(&r)))
}
/// Enumerate all targets in system order for the recovery barrier.
pub(crate) async fn list(ex:&(impl Executor+?Sized))->Result<Vec<CaptureRow>,db::Error> {
    Ok(ex.query(&format!("SELECT {COLS} FROM game.sov_capture_targets ORDER BY system_id,target_item_id"),&[]).await?.iter().map(CaptureRow::from_row).collect())
}
pub(crate) async fn insert(ex:&(impl Executor+?Sized), r:&CaptureRow)->Result<bool,db::Error> {
    Ok(ex.execute("INSERT INTO game.sov_capture_targets (target_item_id,system_id,ownership_epoch,controlling_team,scoring_team,defender_team,score,rate,blocked,score_sim_time,checkpoint_wall_time,revision,capture_time_seconds,defensive_regen_rate,checkpoint_interval_ms,content_namespace,content_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) ON CONFLICT DO NOTHING",&[&r.target_item_id,&r.system_id,&r.ownership_epoch,&r.controlling_team,&r.scoring_team,&r.defender_team,&r.score,&r.rate,&r.blocked,&r.score_sim_time,&r.checkpoint_wall_time,&r.revision,&r.capture_time_seconds,&r.defensive_regen_rate,&r.checkpoint_interval_ms,&r.content_namespace,&r.content_version]).await?==1)
}
pub(crate) async fn checkpoint(ex:&(impl Executor+?Sized), expected_revision:i64, r:&CaptureRow)->Result<bool,db::Error> {
    Ok(ex.execute("UPDATE game.sov_capture_targets SET controlling_team=$5,scoring_team=$6,defender_team=$7,score=$8,rate=$9,blocked=$10,score_sim_time=$11,checkpoint_wall_time=$12,revision=revision+1 WHERE target_item_id=$1 AND system_id=$2 AND ownership_epoch=$3 AND revision=$4 AND content_namespace=$13 AND content_version=$14",&[&r.target_item_id,&r.system_id,&r.ownership_epoch,&expected_revision,&r.controlling_team,&r.scoring_team,&r.defender_team,&r.score,&r.rate,&r.blocked,&r.score_sim_time,&r.checkpoint_wall_time,&r.content_namespace,&r.content_version]).await?==1)
}
pub(crate) async fn remove(ex:&(impl Executor+?Sized), target:i64)->Result<bool,db::Error> {
    Ok(ex.execute("DELETE FROM game.sov_capture_targets WHERE target_item_id=$1",&[&target]).await?==1)
}
