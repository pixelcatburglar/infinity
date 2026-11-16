//! Original-envelope validation and fact extraction for index sources.
//! No client module is mirrored; envelopes come from `game.progress_events` as
//! decoded by `progress::codec`. Business identity lives in the source fields
//! (`business` namespace, `kind:first` keys), never the random event UUID.
//! Pre-first-activation slices and proven non-natural origins are eligibility
//! outcomes of the replay path, not identity failures here.

use crate::{lua::events::{resources::{FactContext, FactOrigin}, GameEvent}, progress::{codec, EventEnvelope}, sde::Sde, services::{db_err, CallError}};
use crate::dao::progress::sovereignty::{FactOrder, Increment};
use super::model::{SourceSlice, SystemFact};

/// Every producer writes business-keyed events under this namespace.
const BUSINESS_NAMESPACE: &str = "business";
/// NPC death business keys are `NpcDestroyed:incarnation/item/token`.
const NPC_KIND: &str = "NpcDestroyed";

fn invalid(reason: impl std::fmt::Display) -> CallError {
    db_err(anyhow::anyhow!("invalid development-index source: {reason}"))
}

/// Canonical envelope metadata must agree with the decoded payload before any
/// origin judgement is trusted: stored context, lowercase origin and the
/// business namespace all bind the same envelope.
fn proven_origin(envelope: &EventEnvelope, context: &FactContext) -> Result<FactOrigin, CallError> {
    if envelope.schema_version != codec::SCHEMA_VERSION || envelope.kind != envelope.event.kind() {
        return Err(invalid("schema or kind mismatch"));
    }
    let decoded = codec::decode(envelope.schema_version, &envelope.kind, &envelope.payload)
        .map_err(|_| invalid("invalid payload"))?;
    if decoded != envelope.event {
        return Err(invalid("payload and event mismatch"));
    }
    let stored: FactContext = serde_json::from_str(&envelope.context).map_err(|_| invalid("stored context"))?;
    if stored != *context || envelope.origin != stored.origin.as_str() || envelope.source_namespace != BUSINESS_NAMESPACE {
        return Err(invalid("context, origin or namespace mismatch"));
    }
    Ok(stored.origin)
}

fn order(envelope: &EventEnvelope, occurred_at: i64, slice_id: i64) -> FactOrder {
    FactOrder {
        occurred_at,
        source_namespace: envelope.source_namespace.clone(),
        source_key: envelope.source_key.clone(),
        ordinal: envelope.ordinal,
        slice_id,
    }
}

/// `death_id` is `NpcDestroyed:` + the 16-byte incarnation + `/` + item + `/` +
/// combat token; the item is temporary, so the identity must keep all parts.
fn npc_death(envelope: &EventEnvelope, death_id: &str, incarnation: &[u8; 16], item: i64) -> Result<(), CallError> {
    let prefix = format!("{NPC_KIND}:{:032x}/{item}/", u128::from_be_bytes(*incarnation));
    let valid = death_id.strip_prefix(&prefix).is_some_and(|token| {
        !token.is_empty() && (token.len() == 1 || !token.starts_with('0'))
            && token.bytes().all(|byte| byte.is_ascii_digit()) && token.parse::<u64>().is_ok()
    });
    if !valid {
        return Err(invalid("non-canonical NPC death identity"));
    }
    if envelope.source_key != death_id || envelope.ordinal != 0 {
        return Err(invalid("NPC death source identity"));
    }
    Ok(())
}

/// Validate one original envelope and flatten it into atomic slices in fact
/// order. `Ok(None)` is a legitimately excluded source: a proven GM or QA
/// origin that may be acknowledged without credit, never an error.
pub(crate) fn map(envelope: &EventEnvelope, sde: &Sde) -> Result<Option<SystemFact>, CallError> {
    match &envelope.event {
        GameEvent::NpcDestroyed { death_id, npc, system_id, killer_id, context, occurred_at, .. } => {
            let origin = proven_origin(envelope, context)?;
            npc_death(envelope, death_id, &npc.incarnation, npc.item_id)?;
            if envelope.subject_character_id != *killer_id || envelope.occurred_at != *occurred_at {
                return Err(invalid("NPC death envelope identity"));
            }
            if context.system_id.is_some_and(|id| id != *system_id) || sde.map.system(*system_id).is_none() {
                return Err(invalid("unknown solar system"));
            }
            if matches!(origin, FactOrigin::Gm | FactOrigin::Qa) {
                return Ok(None);
            }
            Ok(Some(SystemFact { system_id: *system_id, slices: vec![SourceSlice { order: order(envelope, *occurred_at, 0), increment: Increment::NpcDeath }] }))
        }
        GameEvent::OreMined { char_id, context, slices, .. } | GameEvent::GasHarvested { char_id, context, slices, .. } => {
            let origin = proven_origin(envelope, context)?;
            let kind = envelope.event.kind();
            let (first, earliest) = (slices.first(), slices.iter().map(|slice| slice.occurred_at).min());
            let (first, earliest) = match (first, earliest) {
                (Some(first), Some(earliest)) => (first, earliest),
                _ => return Err(invalid("empty mining event")),
            };
            if envelope.source_key != format!("{kind}:{}", first.log_id) || envelope.ordinal < 0
                || envelope.subject_character_id != Some(*char_id) || envelope.occurred_at != earliest {
                return Err(invalid("mining envelope identity"));
            }
            if first.log_id <= 0 || slices.windows(2).any(|pair| pair[0].log_id >= pair[1].log_id) {
                return Err(invalid("mining slice order"));
            }
            let system = context.system_id.ok_or_else(|| invalid("missing mining system"))?;
            if sde.map.system(system).is_none() {
                return Err(invalid("unknown solar system"));
            }
            if matches!(origin, FactOrigin::Gm | FactOrigin::Qa) {
                return Ok(None);
            }
            let mut out: Vec<SourceSlice> = slices.iter()
                .map(|slice| SourceSlice { order: order(envelope, slice.occurred_at, slice.log_id), increment: Increment::MinedM3(slice.volume_m3) })
                .collect();
            out.sort_by(|a, b| a.order.cmp(&b.order));
            Ok(Some(SystemFact { system_id: system, slices: out }))
        }
        _ => Err(invalid("unsupported event kind")),
    }
}
