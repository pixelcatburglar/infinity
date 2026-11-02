//! Persistence for forwardable shared-setting snapshots in `game.shared_setting_links`.
//! This DAO has no direct client mirror: `sharedSettings/util.py:GetDataFromSettingKey`
//! consumes the service result. It performs full-key lookup but applies no access
//! policy; overview links, mutation and revocation are not implemented here.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct NewLink<'a> {
    pub setting_type_id: i32,
    pub hash_value: &'a str,
    pub payload: &'a str,
    pub creator_user_id: i64,
}
pub(crate) struct StoredLink { pub sq_id: i64 }
impl StoredLink { fn from_row(row: &Row) -> Self { Self { sq_id: row.get("sq_id") } } }
pub(crate) struct StoredPayload { pub payload: String }
impl StoredPayload { fn from_row(row: &Row) -> Self { Self { payload: row.get("payload") } } }
pub(crate) struct LinkKey<'a> { pub sq_id: i64, pub hash_value: &'a str, pub setting_type_id: i32 }

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), new: &NewLink<'_>) -> Result<StoredLink, db::Error> {
    let row = ex.query_one("INSERT INTO game.shared_setting_links (kind, setting_type_id, hash_value, payload, creator_user_id) VALUES ('shared', $1, $2, $3, $4) RETURNING sq_id", &[&new.setting_type_id, &new.hash_value, &new.payload, &new.creator_user_id]).await?;
    Ok(StoredLink::from_row(&row))
}

pub(crate) async fn get(ex: &(impl Executor + ?Sized), key: &LinkKey<'_>) -> Result<Option<StoredPayload>, db::Error> {
    Ok(ex.query_opt("SELECT payload FROM game.shared_setting_links WHERE sq_id = $1 AND kind = 'shared' AND hash_value = $2 AND setting_type_id IS NOT DISTINCT FROM $3", &[&key.sq_id, &key.hash_value, &key.setting_type_id]).await?.as_ref().map(StoredPayload::from_row))
}


#[cfg(test)]
mod tests;