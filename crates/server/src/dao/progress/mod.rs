//! Persistent progress journal, subscriptions and delivery receipts in `game`.
//! No client service is mirrored; transactions and delivery policy belong to callers.
//! This module does not retain or delete journal entries.
pub(crate) mod migration;

use crate::{client::filetime_now, db::{executor::Executor, Error}, progress::{codec, DeliveryKey, EventEnvelope}};
use serde::Serialize;
use tokio_postgres::Row;

/// Server retry policy: cap exponential receipt retry at 2^8 seconds.
const RETRY_EXPONENT_CAP: i32 = 8;

#[derive(Clone, Serialize)]
pub(crate) struct JournalEvent {
    pub source_namespace: String,
    pub source_key: String,
    pub schema_version: i32,
    pub kind: String,
    pub occurred_at: i64,
    pub recorded_at: i64,
    pub payload: String,
    pub context: String,
    pub origin: String,
    pub ordinal: i64,
    pub subject_character_id: Option<i64>,
}

struct ConflictRow {
    source_key: String,
    event_id: String,
    stored_kind: String,
    incoming_kind: String,
    matches: bool,
}
impl ConflictRow {
    fn from_row(row: &Row) -> Self {
        Self {
            source_key: row.get("source_key"), event_id: row.get("event_id"),
            stored_kind: row.get("stored_kind"), incoming_kind: row.get("incoming_kind"),
            matches: row.get("matches"),
        }
    }
}

pub(crate) async fn insert_batch(ex: &(impl Executor + ?Sized), events: &[JournalEvent]) -> Result<Vec<(String, String, bool)>, Error> {
    if events.is_empty() { return Ok(Vec::new()); }
    let json = serde_json::to_string(events).map_err(|e| Error::Progress(e.into()))?;
    let inserted = ex.query(
        "INSERT INTO game.progress_events (source_namespace, source_key, schema_version, kind, occurred_at, recorded_at, payload, context, origin, ordinal, subject_character_id) \
         SELECT source_namespace, source_key, schema_version, kind, occurred_at, recorded_at, payload::jsonb, context::jsonb, origin, ordinal, subject_character_id \
         FROM jsonb_to_recordset($1::text::jsonb) AS e(source_namespace text, source_key text, schema_version integer, kind text, occurred_at bigint, recorded_at bigint, payload text, context text, origin text, ordinal bigint, subject_character_id bigint) \
         ON CONFLICT (source_namespace, source_key) DO NOTHING RETURNING source_key, event_id::text AS event_id",
        &[&json],
    ).await?;
    let ids: Vec<String> = inserted.iter().map(|row| row.get("event_id")).collect();
    if !ids.is_empty() {
        ex.execute(
            "INSERT INTO game.progress_deliveries (consumer_key, event_id, state, next_attempt_at, subject_character_id) \
             SELECT s.consumer_key, e.event_id, 'pending', e.recorded_at, e.subject_character_id \
             FROM game.progress_events e JOIN game.progress_subscription_kinds k ON k.kind = e.kind \
             JOIN game.progress_subscriptions s ON s.consumer_key = k.consumer_key AND s.enabled \
             WHERE e.event_id = ANY(SELECT unnest($1::text[])::uuid) ON CONFLICT DO NOTHING",
            &[&ids],
        ).await?;
    }
    if inserted.len() == events.len() {
        return Ok(inserted.into_iter().map(|row| (row.get("source_key"), row.get("event_id"), true)).collect());
    }
    let rows = ex.query(
        "SELECT e.source_key, e.event_id::text AS event_id, e.kind AS stored_kind, i.kind AS incoming_kind, \
         (e.schema_version, e.kind, e.occurred_at, e.payload, e.context, e.origin, e.ordinal, e.subject_character_id) \
         IS NOT DISTINCT FROM (i.schema_version, i.kind, i.occurred_at, i.payload::jsonb, i.context::jsonb, i.origin, i.ordinal, i.subject_character_id) AS matches \
         FROM game.progress_events e JOIN jsonb_to_recordset($1::text::jsonb) AS i(source_namespace text, source_key text, schema_version integer, kind text, occurred_at bigint, payload text, context text, origin text, ordinal bigint, subject_character_id bigint) ON (e.source_namespace,e.source_key)=(i.source_namespace,i.source_key)",
        &[&json],
    ).await?;
    rows.into_iter().map(|row| {
        let row = ConflictRow::from_row(&row);
        if (crate::lua::events::combat::known_kind(&row.stored_kind) || crate::lua::events::combat::known_kind(&row.incoming_kind) || crate::lua::events::goals::known_kind(&row.stored_kind) || crate::lua::events::goals::known_kind(&row.incoming_kind)) && !row.matches {
            return Err(Error::Progress(anyhow::anyhow!("canonical source collision: {}", row.source_key)));
        }
        let is_new = ids.contains(&row.event_id);
        Ok((row.source_key, row.event_id, is_new))
    }).collect()
}

fn envelope(row: &Row) -> Result<EventEnvelope, Error> {
    let version = row.get("schema_version");
    let kind: String = row.get("kind");
    let payload: String = row.get("payload");
    Ok(EventEnvelope {
        event_id: row.get("event_id"), event: codec::decode(version, &kind, &payload).map_err(Error::Progress)?,
        schema_version: version, kind, payload,
        occurred_at: row.get("occurred_at"), recorded_at: row.get("recorded_at"),
        source_namespace: row.get("source_namespace"), source_key: row.get("source_key"),
        ordinal: row.get("ordinal"), context: row.get("context"), causation: row.get("causation"),
        origin: row.get("origin"), subject_character_id: row.get("subject_character_id"),
    })
}

/// Read the original event for a source without claiming or changing deliveries.
pub(crate) async fn source(ex: &(impl Executor + ?Sized), namespace: &str, source_key: &str) -> Result<Option<EventEnvelope>, Error> {
    let row = ex.query_opt(
        "SELECT e.event_id::text AS event_id, e.schema_version, e.kind, e.payload::text AS payload, e.occurred_at, e.recorded_at, e.source_namespace, e.source_key, e.ordinal, e.context::text AS context, e.causation, e.origin, e.subject_character_id \
         FROM game.progress_events e WHERE e.source_namespace = $1 AND e.source_key = $2",
        &[&namespace, &source_key],
    ).await?;
    row.as_ref().map(envelope).transpose()
}

pub(crate) enum DeliveryState {
    Pending(EventEnvelope),
    Applied,
    Missing,
}

/// Read a receipt without locking it, for outside-transaction preparation.
pub(crate) async fn delivery(ex: &(impl Executor + ?Sized), key: &DeliveryKey) -> Result<DeliveryState, Error> {
    let row = ex.query_opt(
        "SELECT d.state, e.event_id::text AS event_id, e.schema_version, e.kind, e.payload::text AS payload, e.occurred_at, e.recorded_at, e.source_namespace, e.source_key, e.ordinal, e.context::text AS context, e.causation, e.origin, e.subject_character_id \
         FROM game.progress_deliveries d JOIN game.progress_events e ON e.event_id = d.event_id \
         WHERE d.consumer_key = $1 AND d.event_id = $2::text::uuid",
        &[&key.consumer_key, &key.event_id],
    ).await?;
    match row {
        None => Ok(DeliveryState::Missing),
        Some(row) if row.get::<_, String>("state") == "pending" => Ok(DeliveryState::Pending(envelope(&row)?)),
        Some(row) if row.get::<_, String>("state") == "applied" => Ok(DeliveryState::Applied),
        Some(_) => Err(Error::Progress(anyhow::anyhow!("unknown progress delivery state"))),
    }
}

/// Snapshot pending event identities in stable order, including the caller's sentinel.
pub(crate) async fn pending_for_character(ex: &(impl Executor + ?Sized), consumer: &str, character: i64, limit: i64) -> Result<Vec<DeliveryKey>, Error> {
    let rows = ex.query(
        "SELECT d.event_id::text AS event_id FROM game.progress_deliveries d \
         WHERE d.consumer_key=$1 AND d.subject_character_id=$2 AND d.state='pending' ORDER BY d.next_attempt_at,d.event_id LIMIT $3",
        &[&consumer, &character, &limit],
    ).await?;
    Ok(rows.into_iter().map(|row| DeliveryKey { consumer_key: consumer.to_owned(), event_id: row.get("event_id") }).collect())
}

pub(crate) async fn lock_pending(ex: &(impl Executor + ?Sized), key: &DeliveryKey) -> Result<Option<EventEnvelope>, Error> {
    let row = ex.query_opt(
        "SELECT d.state, e.event_id::text AS event_id, e.schema_version, e.kind, e.payload::text AS payload, e.occurred_at, e.recorded_at, e.source_namespace, e.source_key, e.ordinal, e.context::text AS context, e.causation, e.origin, e.subject_character_id \
         FROM game.progress_deliveries d JOIN game.progress_events e ON e.event_id = d.event_id \
         WHERE d.consumer_key = $1 AND d.event_id = $2::text::uuid FOR UPDATE OF d",
        &[&key.consumer_key, &key.event_id],
    ).await?;
    match row {
        Some(row) if row.get::<_, String>("state") == "pending" => envelope(&row).map(Some),
        Some(row) if row.get::<_, String>("state") == "applied" => Ok(None),
        Some(_) => Err(Error::Progress(anyhow::anyhow!("unknown progress delivery state"))),
        None => Err(Error::Progress(anyhow::anyhow!("progress delivery does not exist"))),
    }
}

pub(crate) async fn mark_applied(ex: &(impl Executor + ?Sized), key: &DeliveryKey) -> Result<(), Error> {
    let changed = ex.execute("UPDATE game.progress_deliveries SET state = 'applied', applied_at = $3, last_error = NULL WHERE consumer_key = $1 AND event_id = $2::text::uuid AND state = 'pending'", &[&key.consumer_key, &key.event_id, &filetime_now()]).await?;
    if changed != 1 { return Err(Error::Progress(anyhow::anyhow!("progress receipt changed while locked"))); }
    Ok(())
}

pub(crate) async fn reconcile(ex: &(impl Executor + ?Sized), registry: &crate::progress::Registry) -> Result<(), Error> {
    let rows = ex.query("SELECT consumer_key FROM game.progress_subscriptions WHERE enabled", &[]).await?;
    for row in rows {
        let key: String = row.get("consumer_key");
        if registry.registration(&key).is_none() {
            return Err(Error::Progress(anyhow::anyhow!("enabled progress consumer {key} has no handler")));
        }
    }
    for registration in registry.durable() {
        ex.execute(
            "INSERT INTO game.progress_subscriptions (consumer_key,generation,activated_at,version,enabled) VALUES ($1,1,$2,1,true) ON CONFLICT (consumer_key) DO NOTHING",
            &[&registration.consumer_key, &filetime_now()],
        ).await?;
        ex.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key=$1", &[&registration.consumer_key]).await?;
        ex.execute("INSERT INTO game.progress_subscription_kinds (consumer_key,kind) SELECT $1,unnest($2::text[])", &[&registration.consumer_key, &registration.kinds]).await?;
    }
    Ok(())
}

pub(crate) async fn due(ex: &(impl Executor + ?Sized), now: i64, limit: i64, consumer_keys: &[&str]) -> Result<Vec<DeliveryKey>, Error> {
    let rows = ex.query(
        "SELECT d.consumer_key,d.event_id::text AS event_id FROM game.progress_deliveries d JOIN game.progress_subscriptions s ON s.consumer_key=d.consumer_key AND s.enabled WHERE d.state='pending' AND d.next_attempt_at <= $1 AND d.consumer_key = ANY($3) ORDER BY d.next_attempt_at,d.consumer_key,d.event_id LIMIT $2",
        &[&now, &limit, &consumer_keys],
    ).await?;
    Ok(rows.into_iter().map(|r| DeliveryKey { consumer_key: r.get("consumer_key"), event_id: r.get("event_id") }).collect())
}

pub(crate) async fn try_claim(ex: &(impl Executor + ?Sized), key: &DeliveryKey, now: i64) -> Result<bool, Error> {
    Ok(ex.query_opt(
        "SELECT event_id FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid AND state='pending' AND next_attempt_at <= $3 FOR UPDATE SKIP LOCKED",
        &[&key.consumer_key, &key.event_id, &now],
    ).await?.is_some())
}

pub(crate) async fn failed(ex: &(impl Executor + ?Sized), key: &DeliveryKey, now: i64, message: &str) -> Result<(), Error> {
    ex.execute(
        "UPDATE game.progress_deliveries SET attempts = LEAST(attempts::bigint + 1, $5::bigint)::integer, last_error = $3, next_attempt_at = $4 + ($6::bigint * (1::bigint << LEAST(attempts, $7))) WHERE consumer_key = $1 AND event_id = $2::text::uuid AND state = 'pending'",
        &[&key.consumer_key, &key.event_id, &message, &now, &i64::from(i32::MAX), &infinity_core::consts::FILETIME_SECOND, &RETRY_EXPONENT_CAP],
    ).await?;
    Ok(())
}

#[cfg(test)] mod tests;
