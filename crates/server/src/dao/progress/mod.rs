//! Persistent progress journal, subscriptions and delivery receipts in `game`.
//! No client service is mirrored; transactions and delivery policy belong to callers.
//! This module does not retain or delete journal entries; the sovereignty
//! submodule only replays the canonical journal for development indices.
pub(crate) mod migration;
pub(crate) mod sovereignty;

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

pub(crate) async fn insert_batch(
    ex: &(impl Executor + ?Sized), events: &[JournalEvent],
    subscription_consumers: &[&str], subscription_kinds: &[&str],
) -> Result<Vec<(String, String, bool)>, Error> {
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
             FROM game.progress_events e \
             JOIN unnest($2::text[], $3::text[]) AS s(consumer_key, kind) ON s.kind = e.kind \
             WHERE e.event_id = ANY(SELECT unnest($1::text[])::uuid) ON CONFLICT DO NOTHING",
            &[&ids, &subscription_consumers, &subscription_kinds],
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

/// A subscription row read back at startup or in test fixture assembly.
pub(crate) struct SubscriptionRow {
    pub consumer_key: String,
    pub enabled: bool,
    pub kinds: Vec<String>,
}
impl SubscriptionRow {
    fn from_row(row: &Row) -> Self {
        Self { consumer_key: row.get("consumer_key"), enabled: row.get("enabled"), kinds: row.get("kinds") }
    }
}

/// Read the flag and declared kinds of the requested consumers. Startup
/// reconciliation and fixture assembly only; runtime delivery never queries
/// the subscription tables.
pub(crate) async fn subscriptions(ex: &(impl Executor + ?Sized), consumer_keys: &[&str]) -> Result<Vec<SubscriptionRow>, Error> {
    if consumer_keys.is_empty() { return Ok(Vec::new()); }
    let rows = ex.query(
        "SELECT s.consumer_key, s.enabled, ARRAY(SELECT k.kind FROM game.progress_subscription_kinds k WHERE k.consumer_key = s.consumer_key ORDER BY k.kind) AS kinds \
         FROM game.progress_subscriptions s WHERE s.consumer_key = ANY($1) ORDER BY s.consumer_key",
        &[&consumer_keys],
    ).await?;
    Ok(rows.iter().map(SubscriptionRow::from_row).collect())
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

/// Read canonical journal events without regard to their delivery state.
pub(crate) async fn envelopes(ex: &(impl Executor + ?Sized), event_ids: &[String]) -> Result<Vec<EventEnvelope>, Error> {
    if event_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex.query(
        "SELECT e.event_id::text AS event_id, e.schema_version, e.kind, e.payload::text AS payload, e.occurred_at, e.recorded_at, e.source_namespace, e.source_key, e.ordinal, e.context::text AS context, e.causation, e.origin, e.subject_character_id \
         FROM game.progress_events e WHERE e.event_id = ANY(SELECT unnest($1::text[])::uuid) ORDER BY e.event_id",
        &[&event_ids],
    ).await?;
    rows.iter().map(envelope).collect()
}

pub(crate) enum DeliveryState {
    Pending(EventEnvelope),
    Applied,
    Terminal,
    Missing,
}

/// Decode the receipt state of one joined delivery/event row; only pending
/// receipts decode their payload.
fn state(row: &Row) -> Result<DeliveryState, Error> {
    match row.get::<_, String>("state").as_str() {
        "pending" => Ok(DeliveryState::Pending(envelope(row)?)),
        "applied" => Ok(DeliveryState::Applied),
        "terminal" => Ok(DeliveryState::Terminal),
        _ => Err(Error::Progress(anyhow::anyhow!("unknown progress delivery state"))),
    }
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
        Some(row) => state(&row),
    }
}

/// Snapshot pending event identities in stable order, including the caller's sentinel.
/// A consumer outside the enabled projection yields no rows and no query.
pub(crate) async fn pending_for_character(ex: &(impl Executor + ?Sized), consumer: &str, character: i64, limit: i64, enabled_consumers: &[&str]) -> Result<Vec<DeliveryKey>, Error> {
    if !enabled_consumers.contains(&consumer) { return Ok(Vec::new()); }
    let rows = ex.query(
        "SELECT d.event_id::text AS event_id FROM game.progress_deliveries d \
         WHERE d.consumer_key=$1 AND d.subject_character_id=$2 AND d.state='pending' ORDER BY d.next_attempt_at,d.event_id LIMIT $3",
        &[&consumer, &character, &limit],
    ).await?;
    Ok(rows.into_iter().map(|row| DeliveryKey { consumer_key: consumer.to_owned(), event_id: row.get("event_id") }).collect())
}

/// Lock one receipt row and decode its state.
pub(crate) async fn lock_delivery(ex: &(impl Executor + ?Sized), key: &DeliveryKey) -> Result<DeliveryState, Error> {
    let row = ex.query_opt(
        "SELECT d.state, e.event_id::text AS event_id, e.schema_version, e.kind, e.payload::text AS payload, e.occurred_at, e.recorded_at, e.source_namespace, e.source_key, e.ordinal, e.context::text AS context, e.causation, e.origin, e.subject_character_id \
         FROM game.progress_deliveries d JOIN game.progress_events e ON e.event_id = d.event_id \
         WHERE d.consumer_key = $1 AND d.event_id = $2::text::uuid FOR UPDATE OF d",
        &[&key.consumer_key, &key.event_id],
    ).await?;
    match row {
        None => Ok(DeliveryState::Missing),
        Some(row) => state(&row),
    }
}

pub(crate) async fn mark_applied(ex: &(impl Executor + ?Sized), key: &DeliveryKey) -> Result<(), Error> {
    let changed = ex.execute("UPDATE game.progress_deliveries SET state = 'applied', applied_at = $3, last_error = NULL WHERE consumer_key = $1 AND event_id = $2::text::uuid AND state = 'pending'", &[&key.consumer_key, &key.event_id, &filetime_now()]).await?;
    if changed != 1 { return Err(Error::Progress(anyhow::anyhow!("progress receipt changed while locked"))); }
    Ok(())
}

pub(crate) async fn reconcile(ex: &(impl Executor + ?Sized), registry: &crate::progress::Registry) -> Result<Vec<SubscriptionRow>, Error> {
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
    let keys: Vec<&str> = registry.durable().iter().map(|registration| registration.consumer_key).collect();
    subscriptions(ex, &keys).await
}

/// One due receipt and the failure-recording observation token read with it.
pub(crate) struct DeliveryAttempt {
    pub key: DeliveryKey,
    pub attempts: i32,
    pub next_attempt_at: i64,
}
impl DeliveryAttempt {
    fn from_row(row: &Row) -> Self {
        Self { key: DeliveryKey { consumer_key: row.get("consumer_key"), event_id: row.get("event_id") }, attempts: row.get("attempts"), next_attempt_at: row.get("next_attempt_at") }
    }
}

/// The counters a recorded failure actually wrote, or `None` when the
/// receipt no longer matches the observed token (applied, terminal or
/// concurrently updated).
pub(crate) struct FailureUpdate {
    pub attempts: i32,
    pub permanent_rejections: i32,
    pub terminal: bool,
}
impl FailureUpdate {
    fn from_row(row: &Row) -> Self {
        Self { attempts: row.get("attempts"), permanent_rejections: row.get("permanent_rejections"), terminal: row.get("terminal") }
    }
}

pub(crate) async fn due(ex: &(impl Executor + ?Sized), now: i64, limit: i64, consumer_keys: &[&str]) -> Result<Vec<DeliveryAttempt>, Error> {
    let rows = ex.query(
        "SELECT d.consumer_key,d.event_id::text AS event_id,d.attempts,d.next_attempt_at FROM game.progress_deliveries d WHERE d.state='pending' AND d.next_attempt_at <= $1 AND d.consumer_key = ANY($3) ORDER BY d.next_attempt_at,d.consumer_key,d.event_id LIMIT $2",
        &[&now, &limit, &consumer_keys],
    ).await?;
    Ok(rows.iter().map(DeliveryAttempt::from_row).collect())
}

pub(crate) async fn try_claim(ex: &(impl Executor + ?Sized), key: &DeliveryKey, now: i64) -> Result<bool, Error> {
    Ok(ex.query_opt(
        "SELECT event_id FROM game.progress_deliveries WHERE consumer_key=$1 AND event_id=$2::text::uuid AND state='pending' AND next_attempt_at <= $3 FOR UPDATE SKIP LOCKED",
        &[&key.consumer_key, &key.event_id, &now],
    ).await?.is_some())
}

/// Record one failed delivery pass against the observation token returned by
/// `due`: the pending row must still carry the observed attempts and next
/// attempt time, so one observation round counts at most one failure.
/// A permanent failure also increments the capped rejection budget; when the
/// previous count already reached `permanent_retry_limit` the receipt turns
/// terminal instead of being rescheduled.
pub(crate) async fn failed(
    ex: &(impl Executor + ?Sized), attempt: &DeliveryAttempt, now: i64,
    message: &str, permanent: bool, permanent_retry_limit: i32,
) -> Result<Option<FailureUpdate>, Error> {
    let row = ex.query_opt(
        "UPDATE game.progress_deliveries SET \
         attempts = LEAST(attempts::bigint + 1, $5::bigint)::integer, \
         permanent_rejections = CASE WHEN $6 THEN LEAST(permanent_rejections, $7::integer) + 1 ELSE permanent_rejections END, \
         state = CASE WHEN $6 AND permanent_rejections >= $7 THEN 'terminal' ELSE state END, \
         terminal_at = CASE WHEN $6 AND permanent_rejections >= $7 THEN $3 ELSE terminal_at END, \
         next_attempt_at = CASE WHEN $6 AND permanent_rejections >= $7 THEN next_attempt_at ELSE GREATEST($3, next_attempt_at) + ($8::bigint * (1::bigint << LEAST(attempts, $9))) END, \
         last_error = $4 \
         WHERE consumer_key = $1 AND event_id = $2::text::uuid AND state = 'pending' AND attempts = $10 AND next_attempt_at = $11 \
         RETURNING attempts, permanent_rejections, state = 'terminal' AS terminal",
        &[&attempt.key.consumer_key, &attempt.key.event_id, &now, &message, &i64::from(i32::MAX), &permanent, &permanent_retry_limit, &infinity_core::consts::FILETIME_SECOND, &RETRY_EXPONENT_CAP, &attempt.attempts, &attempt.next_attempt_at],
    ).await?;
    Ok(row.map(|row| FailureUpdate::from_row(&row)))
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
