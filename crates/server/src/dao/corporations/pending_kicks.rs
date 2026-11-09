//! Durable corporation kick intent rows. Policy, scheduling, and notifications stay in services.
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct PendingKick {
    pub corporation_id: i64,
    pub character_id: i64,
    pub issuer_id: i64,
    pub requested_at: i64,
    pub eligible_at: Option<i64>,
    pub reason: i16,
}
impl PendingKick {
    fn from_row(row: &Row) -> Self { Self { corporation_id: row.get("corporation_id"), character_id: row.get("character_id"), issuer_id: row.get("issuer_id"), requested_at: row.get("requested_at"), eligible_at: row.get("eligible_at"), reason: row.get("reason") } }
}
pub(crate) async fn get(ex: &(impl Executor + ?Sized), corp: i64, character: i64) -> Result<Option<PendingKick>, db::Error> {
    Ok(ex.query_opt("SELECT corporation_id,character_id,issuer_id,requested_at,eligible_at,reason FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2 FOR UPDATE", &[&corp,&character]).await?.as_ref().map(PendingKick::from_row))
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), corp: i64) -> Result<Vec<PendingKick>, db::Error> {
    Ok(ex.query("SELECT corporation_id,character_id,issuer_id,requested_at,eligible_at,reason FROM game.corp_pending_kicks WHERE corporation_id=$1 ORDER BY character_id", &[&corp]).await?.iter().map(PendingKick::from_row).collect())
}
pub(crate) async fn upsert(ex: &(impl Executor + ?Sized), row: &PendingKick) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.corp_pending_kicks(corporation_id,character_id,issuer_id,requested_at,eligible_at,reason) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(corporation_id,character_id) DO UPDATE SET eligible_at=EXCLUDED.eligible_at,reason=EXCLUDED.reason", &[&row.corporation_id,&row.character_id,&row.issuer_id,&row.requested_at,&row.eligible_at,&row.reason]).await?;
    Ok(())
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), corp: i64, character: i64) -> Result<(), db::Error> {
    ex.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1 AND character_id=$2", &[&corp,&character]).await?;
    Ok(())
}
pub(crate) async fn all(ex: &(impl Executor + ?Sized)) -> Result<Vec<PendingKick>, db::Error> {
    Ok(ex.query("SELECT corporation_id,character_id,issuer_id,requested_at,eligible_at,reason FROM game.corp_pending_kicks ORDER BY corporation_id,character_id", &[]).await?.iter().map(PendingKick::from_row).collect())
}
pub(crate) async fn by_issuer(ex: &(impl Executor + ?Sized), corp: i64, issuer: i64) -> Result<Vec<PendingKick>, db::Error> {
    Ok(ex.query("SELECT corporation_id,character_id,issuer_id,requested_at,eligible_at,reason FROM game.corp_pending_kicks WHERE corporation_id=$1 AND issuer_id=$2 ORDER BY character_id FOR UPDATE", &[&corp, &issuer]).await?.iter().map(PendingKick::from_row).collect())
}
