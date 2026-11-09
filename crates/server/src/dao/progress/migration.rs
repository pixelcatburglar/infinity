//! SQL for the one-time durable progress event schema promotion.
//! No client service is mirrored; only `game.progress_events` is updated.
//! Delivery receipts and event metadata are never changed.

use crate::db::{executor::Executor, Error};

pub(crate) struct LegacyEvent {
    pub event_id: String,
    pub kind: String,
    pub payload: String,
    pub version: i32,
}

impl LegacyEvent {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { event_id: row.get("event_id"), kind: row.get("kind"), payload: row.get("payload"), version: row.get("schema_version") }
    }
}

pub(crate) async fn legacy(ex: &(impl Executor + ?Sized)) -> Result<Vec<LegacyEvent>, Error> {
    let rows = ex.query("SELECT event_id::text AS event_id, kind, payload::text AS payload, schema_version FROM game.progress_events WHERE schema_version IN (1, 2) ORDER BY event_id", &[]).await?;
    Ok(rows.iter().map(LegacyEvent::from_row).collect())
}

pub(crate) async fn promote(ex: &(impl Executor + ?Sized), event: &LegacyEvent, payload: &str, version: i32) -> Result<bool, Error> {
    Ok(ex.execute("UPDATE game.progress_events SET schema_version = $4, payload = $3::text::jsonb WHERE event_id = $1::text::uuid AND schema_version = $5 AND payload = $2::text::jsonb", &[&event.event_id, &event.payload, &payload, &version, &event.version]).await? == 1)
}
