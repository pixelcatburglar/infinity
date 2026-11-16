//! Canonical replay of applied journal facts for sovereignty development indices.
//! Slices are flattened straight from game.progress_events payloads by an
//! indexed LATERAL expansion: one row per NPC death and one row per mining
//! ledger slice, joined to this consumer's receipts; there is no per-slice
//! table, ledger reload or copied payload. Page order is the deterministic fact
//! order (occurred_at, namespace, key, ordinal, slice) with C-collation text
//! keys, so slices of different envelopes interleave and a page never splits a
//! logical position twice. Only natural-origin facts are returned: valid Gm or
//! Qa slices and slices before the caller's start bound stay out even when
//! their receipts were already acknowledged. Malformed payload fields surface
//! as query errors, never as a coerced zero. Callers own eligibility,
//! transactions, acknowledgement and every policy decision.

use crate::db::{self, executor::Executor};

/// Integer-safe system selector shared with progress_events_development_system_idx.
/// Only text that validates as an unsigned bigint within range is cast, so
/// malformed or overflowing historical JSON values can never fail a scan.
const SYSTEM_SQL: &str = "CASE \
    WHEN e.kind = 'NpcDestroyed' AND e.payload->'NpcDestroyed'->>'system_id' ~ '^[0-9]{1,19}$' \
        THEN CASE WHEN (e.payload->'NpcDestroyed'->>'system_id')::numeric <= 9223372036854775807 \
            THEN (e.payload->'NpcDestroyed'->>'system_id')::bigint END \
    WHEN e.kind IN ('OreMined', 'GasHarvested') AND e.context->>'system_id' ~ '^[0-9]{1,19}$' \
        THEN CASE WHEN (e.context->>'system_id')::numeric <= 9223372036854775807 \
            THEN (e.context->>'system_id')::bigint END \
    END";

/// Total order of one atomic source slice; string fields compare in UTF-8/C
/// collation, the same order SQL applies.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FactOrder {
    pub occurred_at: i64,
    pub source_namespace: String,
    pub source_key: String,
    pub ordinal: i64,
    pub slice_id: i64,
}

/// One atomic contribution; NPC deaths carry no magnitude, mining slices carry
/// the frozen cubic metres of their ledger slice.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Increment {
    NpcDeath,
    MinedM3(f64),
}

/// One original journal slice in fact order; `increment` is the frozen contribution.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReplaySlice {
    pub event_id: String,
    pub system_id: i64,
    pub order: FactOrder,
    pub increment: Increment,
}

/// One page of a system's canonical fact order over [from_at, through_at].
/// Returns applied receipts plus the explicitly named pending event; other
/// pending deliveries and every non-natural origin are excluded. `after`
/// resumes strictly after a previously returned order; callers must page to end
/// of facts before acknowledging the current event.
pub(crate) async fn replay_page(
    ex: &(impl Executor + ?Sized),
    consumer: &str,
    system: i64,
    include_pending_event: Option<&str>,
    from_at: i64,
    through_at: i64,
    after: Option<&FactOrder>,
    limit: i64,
) -> Result<Vec<ReplaySlice>, db::Error> {
    if system <= 0 || limit <= 0 || from_at > through_at {
        return Err(db::Error::Progress(anyhow::anyhow!("invalid development replay arguments")));
    }
    let (after_at, after_namespace, after_key, after_ordinal, after_slice) = match after {
        Some(order) => (Some(order.occurred_at), Some(order.source_namespace.as_str()), Some(order.source_key.as_str()), Some(order.ordinal), Some(order.slice_id)),
        None => (None, None, None, None, None),
    };
    let pending: Option<&str> = include_pending_event;
    let sql = format!(
        "SELECT e.event_id::text AS event_id, {SYSTEM_SQL} AS system_id, \
            slice.slice_at AS occurred_at, e.source_namespace, e.source_key, e.ordinal, slice.slice_id, slice.npc_death, slice.volume_m3 \
         FROM game.progress_deliveries d \
         JOIN game.progress_events e ON e.event_id = d.event_id \
         CROSS JOIN LATERAL ( \
            SELECT e.occurred_at AS slice_at, 0::bigint AS slice_id, true AS npc_death, 0.0::float8 AS volume_m3 \
            WHERE e.kind = 'NpcDestroyed' \
            UNION ALL \
            SELECT (s.value->>'occurred_at')::bigint, (s.value->>'log_id')::bigint, false, (s.value->>'volume_m3')::float8 \
            FROM jsonb_array_elements(e.payload -> e.kind -> 'slices') AS s(value) \
            WHERE e.kind IN ('OreMined', 'GasHarvested') \
         ) AS slice \
         WHERE d.consumer_key = $1 \
            AND e.kind IN ('NpcDestroyed', 'OreMined', 'GasHarvested') \
            AND (d.state = 'applied' OR e.event_id = $2::text::uuid) \
            AND e.origin = 'natural' \
            AND {SYSTEM_SQL} = $3 \
            AND slice.slice_at >= $4 AND slice.slice_at <= $5 \
            AND ($6::bigint IS NULL OR (slice.slice_at, e.source_namespace COLLATE \"C\", e.source_key COLLATE \"C\", e.ordinal, slice.slice_id) > ($6::bigint, $7::text, $8::text, $9::bigint, $10::bigint)) \
         ORDER BY slice.slice_at, e.source_namespace COLLATE \"C\", e.source_key COLLATE \"C\", e.ordinal, slice.slice_id \
         LIMIT $11",
    );
    let rows = ex.query(&sql, &[
        &consumer, &pending, &system, &from_at, &through_at,
        &after_at, &after_namespace, &after_key, &after_ordinal, &after_slice, &limit,
    ]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ReplaySlice {
            event_id: row.try_get("event_id")?,
            system_id: row.try_get("system_id")?,
            order: FactOrder {
                occurred_at: row.try_get("occurred_at")?,
                source_namespace: row.try_get("source_namespace")?,
                source_key: row.try_get("source_key")?,
                ordinal: row.try_get("ordinal")?,
                slice_id: row.try_get("slice_id")?,
            },
            increment: if row.try_get::<_, bool>("npc_death")? { Increment::NpcDeath } else { Increment::MinedM3(row.try_get("volume_m3")?) },
        });
    }
    Ok(out)
}

#[cfg(all(test, feature = "tests-storage"))]
mod tests;
