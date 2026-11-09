//! Immutable sovereignty transitions and operation deduplication; no event delivery.
use crate::db::{self,executor::Executor};
#[derive(Clone,Debug,PartialEq)]
pub(crate) struct HistoryRow {
    pub history_id:i64, pub system_id:i64, pub hub_item_id:i64,
    pub old_corporation_id:Option<i64>, pub new_corporation_id:Option<i64>,
    pub old_alliance_id:Option<i64>, pub new_alliance_id:Option<i64>, pub old_epoch:i64, pub new_epoch:i64,
    pub reason:String, pub at:i64, pub actor_id:Option<i64>, pub operation_key:String,
    pub content_namespace:String, pub content_version:String,
}
impl HistoryRow { fn from_row(r:&tokio_postgres::Row)->Self { Self { history_id:r.get("history_id"), system_id:r.get("system_id"), hub_item_id:r.get("hub_item_id"), old_corporation_id:r.get("old_corporation_id"), new_corporation_id:r.get("new_corporation_id"), old_alliance_id:r.get("old_alliance_id"), new_alliance_id:r.get("new_alliance_id"), old_epoch:r.get("old_epoch"), new_epoch:r.get("new_epoch"), reason:r.get("reason"), at:r.get("at"), actor_id:r.get("actor_id"), operation_key:r.get("operation_key"), content_namespace:r.get("content_namespace"), content_version:r.get("content_version") } } }
const COLS:&str="history_id,system_id,hub_item_id,old_corporation_id,new_corporation_id,old_alliance_id,new_alliance_id,old_epoch,new_epoch,reason,at,actor_id,operation_key,content_namespace,content_version";
pub(crate) async fn insert(ex:&(impl Executor+?Sized), r:&HistoryRow)->Result<Option<i64>,db::Error> {
    Ok(ex.query_opt("INSERT INTO game.sov_history (system_id,hub_item_id,old_corporation_id,new_corporation_id,old_alliance_id,new_alliance_id,old_epoch,new_epoch,reason,at,actor_id,operation_key,content_namespace,content_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT (operation_key) DO NOTHING RETURNING history_id",&[&r.system_id,&r.hub_item_id,&r.old_corporation_id,&r.new_corporation_id,&r.old_alliance_id,&r.new_alliance_id,&r.old_epoch,&r.new_epoch,&r.reason,&r.at,&r.actor_id,&r.operation_key,&r.content_namespace,&r.content_version]).await?.map(|r|r.get("history_id")))
}
pub(crate) async fn recent(ex:&(impl Executor+?Sized))->Result<Vec<HistoryRow>,db::Error> {
    Ok(ex.query(&format!("SELECT {COLS} FROM game.sov_history WHERE old_alliance_id IS DISTINCT FROM new_alliance_id ORDER BY at DESC,history_id DESC"),&[]).await?.iter().map(HistoryRow::from_row).collect())
}
