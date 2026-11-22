//! Frozen player-loss facts built from the loss transaction's actual inputs.
//! Retail averages are binary64 observations.
use std::{collections::HashMap, sync::{Arc, Mutex}};
use crate::{db::outbox::{Outbox, SourceKey}, lua::events::{GameEvent, combat::{BinaryAmount, CombatEntity, ConflictKind, KillParticipant, LossValuation, ValuationCategory, ValuationComponent, ValuationSource}, resources::FactContext}, services::{combat::damage::DamageSource, identity::Identity, CallError}, space::{combat::ContributionEntry, snapshot::FactCheckpoint}};
use super::LossOutcome;

pub(crate) struct PlayerLossContext {
    pub checkpoint: Option<Arc<FactCheckpoint>>,
    pub prepared: Mutex<Option<PreparedLoss>>,
    pub new_condition: [f64; 3],
    pub recovery_seed: u64,
    pub recovery_source: Mutex<Option<crate::dao::repairable_ship::LossSource>>,
}

impl PlayerLossContext {
    pub(crate) fn new(checkpoint: Option<Arc<FactCheckpoint>>, new_condition: [f64; 3]) -> Self {
        Self { checkpoint, prepared: Mutex::new(None), new_condition,
            recovery_seed: crate::handshake::random_u64(), recovery_source: Mutex::new(None) }
    }

    pub(crate) fn remember_source(&self, source: crate::dao::repairable_ship::LossSource) -> Result<(), CallError> {
        let mut stored = crate::poison::lock_or_exit(self.recovery_source.lock());
        match stored.as_ref() {
            Some(previous) if previous != &source => Err(crate::services::db_err(crate::db::Error::StaleSnapshot("loss source changed"))),
            Some(_) => Ok(()),
            None => { *stored = Some(source); Ok(()) }
        }
    }
}

pub(in crate::services) fn loss_identity(system: i64, snapshot: &crate::space::combat::FirstLethalSnapshot) -> Option<Identity> {
    snapshot.identity.map(|char_id| Identity {
        char_id, ship_id: Some(snapshot.item_id), corp_id: snapshot.corp_id,
        station_id: None, system_id: Some(system),
    })
}

/// Freeze the original physical root after its identity guards have passed.
pub(in crate::services) async fn freeze_source(
    ex: &(impl crate::db::executor::Executor + ?Sized), pending: &PlayerLossContext,
    kind: crate::dao::repairable_ship::SourceKind, item: i64, owner: i64, type_id: i32,
    system: i64, lethal_at: i64, position: [f64; 3],
) -> Result<crate::dao::repairable_ship::LossSource, CallError> {
    use crate::dao::repairable_ship::{ItemId, LossSource};
    let id = ItemId::new(item).ok_or_else(|| CallError::Internal(anyhow::anyhow!("invalid loss item")))?;
    let mut rows = crate::dao::items::repairable_ship::capture_many(ex, &[id]).await.map_err(crate::services::db_err)?;
    let shape = rows.pop().ok_or_else(|| crate::services::db_err(crate::db::Error::StaleSnapshot("loss root missing")))?;
    if shape.item != id || shape.owner != owner || shape.type_id != type_id || shape.location != system
        || shape.flag != 0 || !shape.singleton || shape.quantity != 1 || shape.brain_slot.is_some()
        || shape.jump_clone.is_some() || ![shape.x, shape.y, shape.z].iter().all(|v| v.is_some_and(f64::is_finite))
        || !position.iter().all(|v| v.is_finite()) {
        return Err(crate::services::db_err(crate::db::Error::StaleSnapshot("loss root changed")));
    }
    let source = LossSource { kind, beneficiary: owner, shape, system, lethal_at, sampled_position: position };
    pending.remember_source(source.clone())?;
    Ok(source)
}

#[derive(Clone)]
pub(crate) struct PreparedLoss {
    pub event: GameEvent,
    pub outcome: LossOutcome,
    /// Body-loss refresh categories the reconcile replays; absent for ordinary hulls.
    pub(in crate::services) body: Option<crate::services::skills::body_change::BodyRefresh>,
}

pub(super) fn valuation(ship_type: i32, items: &crate::services::crimewatch::Lost, prices: &HashMap<i32, f64>, observed_at: i64, payout: crate::isk::IskAmount) -> Result<LossValuation, CallError> {
    let component = |category, type_id, quantity| -> Result<ValuationComponent, CallError> {
        let unit_price = prices.get(&type_id).map(|price| BinaryAmount::from_f64(*price)).transpose().map_err(CallError::Internal)?;
        Ok(ValuationComponent { category, type_id, quantity, unit_price })
    };
    let mut components = Vec::with_capacity(items.len() + 1);
    components.push(component(ValuationCategory::Hull, ship_type, 1)?);
    for &(type_id, flag, _, quantity) in items {
        let category = if flag == infinity_core::consts::FLAG_IMPLANT { ValuationCategory::DestroyedImplant }
            else if crate::services::inventory::is_fitting_flag(flag) { ValuationCategory::Fitting } else { ValuationCategory::Cargo };
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

#[cfg(all(test, feature = "tests-ships"))] mod tests;
