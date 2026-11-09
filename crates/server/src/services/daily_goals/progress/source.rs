//! Pure projection of committed native progress facts into daily-goal inputs.
//! The client contribution methods are described by `dailygoals/client/utils.py`;
//! inputs are journal envelopes and immutable SDE map records. Legacy events,
//! gas, hacking and completion events are not natural daily sources.
use crate::{
    db::outbox::SourceKey,
    lua::{content::daily_goals::{model::Owner, plans::FactInput}, events::{
        GameEvent, combat::{BinaryAmount, CombatEntity, HpLayer},
        resources::{FactContext, FactOrigin, LoyaltyReason},
    }},
    progress::{codec::SCHEMA_VERSION, EventEnvelope},
    sde::Sde,
    services::CallError,
};

pub(super) const KINDS: &[&str] = &[
    "NpcDestroyed", "DamageApplied", "RemoteRepairApplied", "OreMined",
    "SignatureResolved", "ManufacturingInstalled", "WreckSalvaged",
    "LoyaltyPointsEarned", "Jumped",
];

fn method(kind: &str, layer: Option<HpLayer>) -> Option<&'static str> {
    match (kind, layer) {
        ("NpcDestroyed", _) => Some("KillNPC"),
        ("DamageApplied", _) => Some("DamageShip"),
        ("RemoteRepairApplied", Some(HpLayer::Shield)) => Some("RemoteRepairShields"),
        ("RemoteRepairApplied", Some(HpLayer::Armor)) => Some("RemoteRepairArmor"),
        ("OreMined", _) => Some("MineOre"),
        ("SignatureResolved", _) => Some("ScanSignature"),
        ("ManufacturingInstalled", _) => Some("ManufactureItem"),
        ("WreckSalvaged", _) => Some("SalvageWreck"),
        ("LoyaltyPointsEarned", _) => Some("EarnLoyaltyPoints"),
        ("Jumped", _) => Some("SpaceJump"),
        _ => None,
    }
}

pub(crate) fn supports(method_key: &str) -> bool {
    KINDS.iter().any(|kind| [None, Some(HpLayer::Shield), Some(HpLayer::Armor), Some(HpLayer::Hull)]
        .into_iter().any(|layer| method(kind, layer) == Some(method_key)))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Increment { Units(u64), Hp(BinaryAmount) }
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Slice { pub(super) occurred_at: i64, pub(super) amount: Increment }
#[derive(Debug)]
pub(super) struct Fact { pub(super) character: i64, pub(super) input: FactInput, pub(super) slices: Vec<Slice> }

fn error(message: &'static str) -> CallError { CallError::Internal(anyhow::anyhow!(message)) }
fn id(value: i64) -> Result<u64, CallError> {
    u64::try_from(value).ok().filter(|v| *v > 0).ok_or_else(|| error("invalid daily source identifier"))
}
fn optional_id(value: Option<i64>) -> Result<Option<u64>, CallError> { value.map(id).transpose() }
fn units(at: i64, amount: u64) -> Vec<Slice> { vec![Slice { occurred_at: at, amount: Increment::Units(amount) }] }
fn target(input: &mut FactInput, entity: &CombatEntity) -> Result<(), CallError> {
    input.type_id = Some(entity.type_id);
    input.character = optional_id(entity.character_id)?;
    input.corporation = optional_id(entity.corporation_id)?;
    input.alliance = optional_id(entity.alliance_id)?;
    input.faction = optional_id(entity.faction_id)?;
    Ok(())
}
fn source_key(e: &EventEnvelope, key: &SourceKey) -> Result<(), CallError> {
    if e.source_namespace != "business" || e.source_key != key.as_str() { return Err(error("daily source identity mismatch")); }
    Ok(())
}
fn invocation(e: &EventEnvelope) -> Result<(), CallError> {
    if e.source_namespace == "business" || e.source_namespace.is_empty() || e.source_key != e.ordinal.to_string() || e.ordinal < 0 {
        return Err(error("daily invocation identity mismatch"));
    }
    Ok(())
}

pub(super) fn map(e: &EventEnvelope, sde: &Sde) -> Result<Option<Fact>, CallError> {
    if e.schema_version != SCHEMA_VERSION || e.kind != e.event.kind() || !KINDS.contains(&e.kind.as_str()) || e.occurred_at <= 0 || e.ordinal < 0 {
        return Err(error("invalid daily event envelope"));
    }
    let metadata = e.event.resource_metadata().or_else(|| crate::lua::events::combat::metadata(&e.event));
    let context = if let Some((at, context)) = metadata {
        let stored: FactContext = serde_json::from_str(&e.context).map_err(|err| CallError::Internal(err.into()))?;
        if stored != *context || e.origin != context.origin.as_str() || e.occurred_at != at {
            return Err(error("daily event metadata mismatch"));
        }
        Some(*context)
    } else if matches!(e.event, GameEvent::Jumped { .. }) {
        let value: serde_json::Value = serde_json::from_str(&e.context).map_err(|err| CallError::Internal(err.into()))?;
        if value != serde_json::json!({}) || e.origin != "server" { return Err(error("daily jump metadata mismatch")); }
        None
    } else { return Err(error("missing daily event metadata")); };

    let mut input = FactInput::default();
    if let Some(ctx) = context { input.system = optional_id(ctx.system_id)?; }
    let layer = match &e.event { GameEvent::RemoteRepairApplied { layer, .. } => Some(*layer), _ => None };
    let projected_method = method(&e.kind, layer);
    let (character, slices) = match &e.event {
        GameEvent::NpcDestroyed { death_id, npc, system_id, killer_id, killer, occurred_at, .. } => {
            if e.source_namespace != "business" || e.source_key != *death_id || e.ordinal != 0 { return Err(error("daily npc source mismatch")); }
            if e.subject_character_id != *killer_id || killer.as_ref().and_then(|v| v.character_id) != *killer_id { return Err(error("daily npc killer mismatch")); }
            let Some(character) = killer_id else { return Ok(None); };
            input.system = Some(id(*system_id)?); target(&mut input, npc)?;
            (*character, units(*occurred_at, 1))
        }
        GameEvent::DamageApplied { char_id, source, target: recipient, slices, .. } | GameEvent::RemoteRepairApplied { char_id, source, target: recipient, slices, .. } => {
            if e.source_namespace != "business" || !(e.source_key.starts_with(&format!("{}:", e.kind)) || (e.kind == "DamageApplied" && e.source_key.starts_with("lethal:"))) { return Err(error("daily combat source mismatch")); }
            if e.subject_character_id != *char_id || source.character_id.is_some_and(|id| Some(id) != *char_id) { return Err(error("daily combat subject mismatch")); }
            let Some(character) = *char_id else { return Ok(None); };
            if projected_method.is_none() { return Ok(None); }
            target(&mut input, recipient)?;
            (character, slices.iter().map(|s| Slice { occurred_at: s.first_occurred_at, amount: Increment::Hp(s.amount.clone()) }).collect())
        }
        GameEvent::OreMined { char_id, ore_type, slices, .. } => {
            let first = slices.first().ok_or_else(|| error("empty daily mining source"))?;
            source_key(e, &SourceKey::new("OreMined", first.log_id))?;
            input.type_id = Some(*ore_type);
            (*char_id, slices.iter().map(|s| Ok(Slice { occurred_at: s.occurred_at, amount: Increment::Units(id(s.quantity)?) })).collect::<Result<Vec<_>, CallError>>()?)
        }
        GameEvent::SignatureResolved { char_id, signature_type, archetype, occurred_at, .. } => {
            invocation(e)?; input.signature = Some(id(*signature_type)?); input.archetype = optional_id(*archetype)?;
            (*char_id, units(*occurred_at, 1))
        }
        GameEvent::ManufacturingInstalled { char_id, job_id, owner_id, owner_is_corporation, product_type, scheduled_quantity, facility_id, occurred_at, .. } => {
            source_key(e, &SourceKey::new("ManufacturingInstalled", *job_id))?;
            if *owner_is_corporation == (*owner_id == *char_id) { return Err(error("daily manufacturing owner mismatch")); }
            input.type_id = Some(*product_type); input.facility = Some(id(*facility_id)?);
            if sde.map.station(*facility_id).is_none() { return Err(error("daily manufacturing facility absent from SDE")); }
            input.facility_is_station = Some(true);
            input.owner = Some(if *owner_is_corporation { Owner::Corporation } else { Owner::Character });
            (*char_id, units(*occurred_at, id(*scheduled_quantity)?))
        }
        GameEvent::WreckSalvaged { char_id, wreck_id, occurred_at, .. } => {
            source_key(e, &SourceKey::new("WreckSalvaged", *wreck_id))?;
            (*char_id, units(*occurred_at, 1))
        }
        GameEvent::LoyaltyPointsEarned { char_id, issuer_id, amount, reason, occurred_at, .. } => {
            invocation(e)?;
            if *reason != LoyaltyReason::MissionReward || *issuer_id == infinity_core::consts::CORP_HERALDRY { return Err(error("invalid daily loyalty source")); }
            input.corporation = Some(id(*issuer_id)?);
            (*char_id, units(*occurred_at, id(*amount)?))
        }
        GameEvent::Jumped { char_id, to_system_id, .. } => {
            if e.source_namespace != "business" || !e.source_key.strip_prefix("Jumped:").and_then(|v| v.parse::<i64>().ok()).is_some_and(|v| v > 0) { return Err(error("daily jump source mismatch")); }
            input.system = Some(id(*to_system_id)?);
            (*char_id, units(e.occurred_at, 1))
        }
        _ => return Err(error("unsupported daily source")),
    };
    input.method = projected_method.ok_or_else(|| error("unsupported daily source"))?.into();
    id(character)?;
    if e.subject_character_id != Some(character) { return Err(error("daily event subject mismatch")); }
    if context.is_some_and(|c| c.origin != FactOrigin::Natural) { return Ok(None); }
    Ok(Some(Fact { character, input, slices }))
}

#[cfg(test)] mod tests;
