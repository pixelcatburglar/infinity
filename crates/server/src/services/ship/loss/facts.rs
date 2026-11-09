//! Frozen player-loss facts built from the loss transaction's actual inputs.
//! Retail averages are binary64 observations; implant destruction is not modelled.
use std::{collections::HashMap, sync::{Arc, Mutex}};
use crate::{db::outbox::{Outbox, SourceKey}, lua::events::{GameEvent, combat::{BinaryAmount, CombatEntity, ConflictKind, KillParticipant, LossValuation, ValuationCategory, ValuationComponent, ValuationSource}, resources::FactContext}, services::{combat::damage::DamageSource, CallError}, space::{combat::ContributionEntry, snapshot::FactCheckpoint}};
use super::LossOutcome;

pub(crate) struct PlayerLossContext {
    pub checkpoint: Option<Arc<FactCheckpoint>>,
    pub prepared: Mutex<Option<PreparedLoss>>,
    pub new_condition: [f64; 3],
}

#[derive(Clone)]
pub(crate) struct PreparedLoss {
    pub event: GameEvent,
    pub outcome: LossOutcome,
}

pub(super) fn valuation(ship_type: i32, items: &crate::services::crimewatch::Lost, prices: &HashMap<i32, f64>, observed_at: i64, payout: crate::isk::IskAmount) -> Result<LossValuation, CallError> {
    let component = |category, type_id, quantity| -> Result<ValuationComponent, CallError> {
        let unit_price = prices.get(&type_id).map(|price| BinaryAmount::from_f64(*price)).transpose().map_err(CallError::Internal)?;
        Ok(ValuationComponent { category, type_id, quantity, unit_price })
    };
    let mut components = Vec::with_capacity(items.len() + 1);
    components.push(component(ValuationCategory::Hull, ship_type, 1)?);
    for &(type_id, flag, _, quantity) in items {
        let category = if crate::services::inventory::is_fitting_flag(flag) { ValuationCategory::Fitting } else { ValuationCategory::Cargo };
        components.push(component(category, type_id, quantity)?);
    }
    Ok(LossValuation { components, source: ValuationSource::RetailPricesAverage, source_revision: None, valuation_observed_at: observed_at, npc_insurance_paid: payout })
}

pub(super) fn events<E>(outbox: &mut Outbox<E>, loss_id: i64, killmail_id: i64, victim: CombatEntity, context: FactContext, occurred_at: i64, conflict: ConflictKind, source: &DamageSource, contributions: &[ContributionEntry], valuation: LossValuation) -> GameEvent {
    let final_blow = source.facts.as_ref().map(|s| s.entity);
    let mut entries: Vec<_> = contributions.iter().filter(|entry| entry.attacker_id > 0 && entry.exact_damage.occurrences() > 0).collect();
    entries.sort_unstable_by_key(|entry| entry.attacker_id);
    let participants: Vec<_> = entries.iter().map(|entry| KillParticipant { character_id: entry.attacker_id, entity: entry.entity, final_blow: final_blow == Some(entry.entity), damage: entry.exact_damage.to_amount() }).collect();
    let event = GameEvent::LossSettled { char_id: victim.character_id.expect("player loss victim"), loss_id, killmail_id, victim, context, occurred_at, final_blow, participants: participants.clone(), conflict, valuation };
    outbox.event_from(SourceKey::canonical("LossSettled", loss_id, 0), event.clone());
    for (entry, participant) in entries.into_iter().zip(participants) {
        outbox.event_from(SourceKey::canonical("PlayerKillCredited", format!("{loss_id}:{}", entry.attacker_id), 0), credit(loss_id, killmail_id, victim, occurred_at, entry, participant));
    }
    event
}

fn credit(loss_id: i64, killmail_id: i64, victim: CombatEntity, occurred_at: i64, entry: &ContributionEntry, participant: KillParticipant) -> GameEvent {
    GameEvent::PlayerKillCredited { char_id: entry.attacker_id, loss_id, killmail_id, attacker: participant.entity, victim, final_blow: participant.final_blow, damage: participant.damage, context: entry.context, occurred_at }
}

#[cfg(test)] mod tests;
