//! Versioned serialization of game events for durable progress delivery.
//! The client does not consume this representation; Lua retains its own event tables.
//! Unknown schemas, malformed events and non-finite values are rejected.

use crate::lua::events::GameEvent;
use crate::lua::events::resources::{FactOrigin, LoyaltyReason};
use std::collections::HashSet;

/// Server-owned event schema with exact wallet and market amounts.
pub(crate) const SCHEMA_VERSION: i32 = 3;

fn validate(event: &GameEvent) -> anyhow::Result<()> {
    if crate::lua::events::combat::kind(event).is_some() {
        return crate::lua::events::combat::validate(event);
    }
    if crate::lua::events::goals::kind(event).is_some() {
        return crate::lua::events::goals::validate(event);
    }
    match event {
        GameEvent::OreMined { slices, .. } | GameEvent::GasHarvested { slices, .. } => {
            anyhow::ensure!(!slices.is_empty(), "empty mining event");
            let mut quantity = 0_i64;
            let mut volume = 0.0_f64;
            let mut ids = HashSet::with_capacity(slices.len());
            for slice in slices {
                anyhow::ensure!(slice.log_id > 0 && ids.insert(slice.log_id) && slice.occurred_at > 0 && slice.quantity > 0 && slice.volume_m3.is_finite() && slice.volume_m3 > 0.0, "invalid mining slice");
                quantity = quantity.checked_add(slice.quantity).ok_or_else(|| anyhow::anyhow!("mining quantity overflow"))?;
                volume += slice.volume_m3;
                anyhow::ensure!(volume.is_finite(), "mining volume overflow");
            }
            Ok(())
        }
        GameEvent::ManufacturingInstalled { runs, scheduled_quantity, occurred_at, .. } => {
            anyhow::ensure!(*runs > 0 && *scheduled_quantity > 0 && *occurred_at > 0, "invalid manufacturing installation");
            Ok(())
        }
        GameEvent::ManufacturingDelivered { quantity, occurred_at, .. } => {
            anyhow::ensure!(*quantity > 0 && *occurred_at > 0, "invalid manufacturing delivery");
            Ok(())
        }
        GameEvent::SignatureResolved { occurred_at, .. } | GameEvent::WreckSalvaged { occurred_at, .. }
        | GameEvent::ContainerHacked { occurred_at, .. } => {
            anyhow::ensure!(*occurred_at > 0, "invalid resource event time");
            Ok(())
        }
        GameEvent::LoyaltyPointsEarned { issuer_id, amount, reason, occurred_at, context, .. } => {
            anyhow::ensure!(*issuer_id != infinity_core::consts::CORP_HERALDRY && *amount > 0 && *reason == LoyaltyReason::MissionReward && context.origin == FactOrigin::Natural && *occurred_at > 0, "invalid loyalty earning");
            Ok(())
        }
        _ => Ok(()),
    }
}

pub(crate) fn encode(event: &GameEvent) -> anyhow::Result<String> {
    validate(event)?;
    Ok(serde_json::to_string(event)?)
}

pub(crate) fn decode(version: i32, kind: &str, payload: &str) -> anyhow::Result<GameEvent> {
    anyhow::ensure!(version == SCHEMA_VERSION, "unknown progress schema version {version}");
    let event: GameEvent = serde_json::from_str(payload)?;
    anyhow::ensure!(event.kind() == kind, "progress event kind does not match payload");
    validate(&event)?;
    Ok(event)
}

#[cfg(test)] mod tests;
