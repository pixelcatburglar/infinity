//! Worker-side conversion of frozen lethal damage into journal events.
//! The source transaction owns durability; no client protocol or live world is read.

use std::fmt::{self, Display, Formatter};
use crate::db::outbox::{Outbox, SourceKey};
use crate::lua::events::{GameEvent, combat::{CombatSlice, SettlementId}};
use super::{PendingDamage, reservation::LAYERS};

struct LethalIdentity { settlement: SettlementId, layer: usize }
impl Display for LethalIdentity {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.settlement.source_instance { write!(f, "{byte:02x}")?; }
        write!(f, ":{}:{}:{}:", self.settlement.generation, self.settlement.sequence, self.settlement.target_id)?;
        for byte in self.settlement.target_incarnation { write!(f, "{byte:02x}")?; }
        write!(f, ":{}", self.layer)
    }
}
impl PendingDamage {
    /// Queue frozen positive layers in the same transaction as the death.
    pub(crate) fn append<E>(&self, outbox: &mut Outbox<E>) {
        for (index, layer) in LAYERS.into_iter().enumerate() {
            let sum = &self.amounts[index];
            if sum.occurrences() == 0 { continue; }
            let identity = LethalIdentity { settlement: self.settlement, layer: index };
            let slice = CombatSlice { index: self.slice, qualification_epoch: self.qualification_epoch,
                boundary_at: self.boundary_at, active_until: self.occurred_at,
                first_occurred_at: self.occurred_at, last_occurred_at: self.occurred_at,
                successful_count: sum.occurrences(), amount: sum.to_amount() };
            outbox.event_from(SourceKey::canonical("lethal", identity, index as i64), GameEvent::DamageApplied {
                char_id: self.source.actual_actor_character, context: self.source.context,
                source: self.source.entity, target: self.target, module_id: self.source.module_id,
                weapon_type_id: self.source.weapon_type_id, effect_id: self.source.effect_id,
                layer, slices: vec![slice],
            });
        }
    }
}

#[cfg(test)] mod tests;
