//! Persistence for client alert reports from carbon/common/script/sys/alert.py
//! and crashes from clientStatsSvc.py. Uses game.client_errors,
//! game.client_error_reports and game.client_crashes; stats are not stored.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct ErrorMode { pub error_id: i64, pub log_mode: String, pub inserted: bool, pub timestamp: String }
impl ErrorMode {
    fn from_row(row: &Row) -> Self {
        Self { error_id: row.get("error_id"), log_mode: row.get("log_mode"), inserted: row.get("inserted"), timestamp: row.get("timestamp") }
    }
}
pub(crate) async fn lookup(ex: &(impl Executor + ?Sized), hash: i64) -> Result<Option<ErrorMode>, db::Error> {
    Ok(ex.query_opt("SELECT error_id, log_mode::text AS log_mode, false AS inserted, ''::text AS timestamp FROM game.client_errors WHERE stack_hash=$1", &[&hash]).await?.as_ref().map(ErrorMode::from_row))
}
pub(crate) async fn upsert(ex: &(impl Executor + ?Sized), hash: i64, key: &str, mode: &str, log_mode: &str, user: i64, character: Option<i64>, trace: &str) -> Result<ErrorMode, db::Error> {
    let row = ex.query_one("INSERT INTO game.client_errors (stack_hash,stack_key,mode,log_mode,first_user_id,first_char_id,traceback) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (stack_hash) DO UPDATE SET last_seen=now(), occurrences=LEAST(game.client_errors.occurrences::numeric + 1, 9223372036854775807)::bigint RETURNING error_id, log_mode::text AS log_mode, (xmax=0) AS inserted, to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS timestamp", &[&hash, &key, &mode, &log_mode, &user, &character, &trace]).await?;
    Ok(ErrorMode::from_row(&row))
}
pub(crate) async fn add_count(ex: &(impl Executor + ?Sized), id: i64, count: i64, seconds: i64) -> Result<(), db::Error> {
    ex.execute("UPDATE game.client_errors SET occurrences=LEAST(occurrences::numeric+$2::bigint::numeric,9223372036854775807)::bigint, last_seen=GREATEST(last_seen,LEAST(to_timestamp($3::double precision),now())) WHERE error_id=$1", &[&id, &count, &(seconds as f64)]).await?;
    Ok(())
}
pub(crate) async fn add_report(ex: &(impl Executor + ?Sized), id: i64, user: i64, character: Option<i64>, count: i64, seconds: i64, arguments: &str) -> Result<String, db::Error> {
    let row = ex.query_one("INSERT INTO game.client_error_reports (error_id,user_id,char_id,count,reported_at,arguments) VALUES ($1,$2,$3,$4,LEAST(to_timestamp($5::double precision),now()),$6) RETURNING to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS timestamp", &[&id, &user, &character, &count, &(seconds as f64), &arguments]).await?;
    Ok(row.get("timestamp"))
}
pub(crate) async fn add_crash(ex: &(impl Executor + ?Sized), user: i64, character: Option<i64>, crash: &str) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.client_crashes (user_id,char_id,crash) VALUES ($1,$2,$3)", &[&user, &character, &crash]).await?;
    Ok(())
}
