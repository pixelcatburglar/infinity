//! Immutable combat facts and their server-side Lua projections.
//! These snapshots are not client protocol objects and contain no live handles.

use mlua::{Lua, Table, Value};
use serde::{Deserialize, Serialize};
use super::{GameEvent, resources::FactContext, fields::{LuaField, set_field}};
pub(crate) mod amount;
use amount::ExactBinarySum;
// Server limit for an optional authoritative static-data version identifier.
const MAX_STATIC_VERSION_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CombatEntity { pub item_id: i64, pub type_id: i32, pub incarnation: [u8; 16], pub character_id: Option<i64>, pub corporation_id: Option<i64>, pub alliance_id: Option<i64>, pub faction_id: Option<i64> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SettlementId { pub source_instance: [u8; 16], pub generation: u64, pub sequence: u64, pub target_id: i64, pub target_incarnation: [u8; 16] }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SettlementKey { pub settlement: SettlementId, pub part: SettlementPart }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SettlementPart { Whole, Shield, Armor, Hull }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HpLayer { Shield, Armor, Hull }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryAmount { pub coefficient: String, pub exponent: i16 }
impl BinaryAmount {
    pub(crate) fn from_f64(value: f64) -> anyhow::Result<Self> {
        anyhow::ensure!(value.is_finite() && value >= 0.0, "invalid binary amount");
        let mut sum = ExactBinarySum::default();
        if value != 0.0 { anyhow::ensure!(sum.try_add(value), "binary amount overflow"); }
        Ok(sum.to_amount())
    }
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        amount::parse(self).map(|_| ())
    }
    fn positive(&self) -> anyhow::Result<()> { self.validate()?; anyhow::ensure!(self.coefficient != "0", "zero combat amount"); Ok(()) }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatSlice { pub index: u64, pub qualification_epoch: u64, pub boundary_at: i64, pub active_until: i64, pub first_occurred_at: i64, pub last_occurred_at: i64, pub successful_count: u64, pub amount: BinaryAmount }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatInterval { pub activation: SettlementKey, pub segment_sequence: u64, pub active_after: i64, pub active_until: i64 }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillParticipant { pub character_id: i64, pub entity: CombatEntity, pub final_blow: bool, pub damage: BinaryAmount }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictKind { PlayerVersusPlayer, Npc, Environment, Unknown }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValuationCategory { Hull, Fitting, Cargo, DestroyedImplant }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValuationComponent { pub category: ValuationCategory, pub type_id: i32, pub quantity: i64, pub unit_price: Option<BinaryAmount> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValuationSource { RetailPricesAverage }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LossValuation { pub components: Vec<ValuationComponent>, pub source: ValuationSource, pub source_revision: Option<String>, pub valuation_observed_at: i64, pub npc_insurance_paid: crate::isk::IskAmount }

pub(crate) fn kind(event: &GameEvent) -> Option<&'static str> {
    Some(match event {
        GameEvent::DamageApplied { .. } => "DamageApplied", GameEvent::RemoteRepairApplied { .. } => "RemoteRepairApplied",
        GameEvent::WarpScrambleApplied { .. } => "WarpScrambleApplied", GameEvent::NpcDestroyed { .. } => "NpcDestroyed",
        GameEvent::LossSettled { .. } => "LossSettled", GameEvent::PlayerKillCredited { .. } => "PlayerKillCredited", _ => return None,
    })
}
pub(crate) fn known_kind(name: &str) -> bool { matches!(name, "DamageApplied" | "RemoteRepairApplied" | "WarpScrambleApplied" | "NpcDestroyed" | "LossSettled" | "PlayerKillCredited") }
pub(crate) fn metadata(event: &GameEvent) -> Option<(i64, &FactContext)> {
    match event {
        GameEvent::DamageApplied { context, slices, .. } | GameEvent::RemoteRepairApplied { context, slices, .. } => slices.iter().map(|s| s.first_occurred_at).min().map(|t| (t, context)),
        GameEvent::WarpScrambleApplied { context, slices, intervals, .. } => slices.iter().map(|s| s.first_occurred_at).chain(intervals.iter().map(|i| i.active_after)).min().map(|t| (t, context)),
        GameEvent::NpcDestroyed { occurred_at, context, .. } | GameEvent::LossSettled { occurred_at, context, .. } | GameEvent::PlayerKillCredited { occurred_at, context, .. } => Some((*occurred_at, context)), _ => None,
    }
}
pub(crate) fn subject(event: &GameEvent) -> Option<i64> {
    match event { GameEvent::DamageApplied { char_id, .. } | GameEvent::RemoteRepairApplied { char_id, .. } | GameEvent::WarpScrambleApplied { char_id, .. } => *char_id, GameEvent::LossSettled { char_id, .. } | GameEvent::PlayerKillCredited { char_id, .. } => Some(*char_id), GameEvent::NpcDestroyed { killer_id, .. } => *killer_id, _ => None }
}
fn entity(e: &CombatEntity) -> anyhow::Result<()> {
    anyhow::ensure!(e.item_id > 0 && e.type_id > 0 && [e.character_id, e.corporation_id, e.alliance_id, e.faction_id].iter().flatten().all(|&id| id > 0), "invalid combat entity"); Ok(())
}
fn slices_valid(slices: &[CombatSlice]) -> anyhow::Result<()> {
    anyhow::ensure!(!slices.is_empty(), "empty combat slices");
    for (i, s) in slices.iter().enumerate() {
        anyhow::ensure!(s.boundary_at > 0 && s.boundary_at <= s.first_occurred_at && s.first_occurred_at <= s.last_occurred_at && s.last_occurred_at <= s.active_until && s.successful_count > 0, "invalid combat slice");
        s.amount.positive()?;
        if i > 0 { let p = &slices[i-1]; anyhow::ensure!(p.index < s.index && p.qualification_epoch <= s.qualification_epoch && p.active_until <= s.boundary_at, "overlapping combat slices"); }
    }
    Ok(())
}
fn source_valid(char_id: Option<i64>, source: &CombatEntity, module_id: Option<i64>, effect_id: Option<i64>) -> bool {
    char_id.is_none_or(|id| id > 0) && source.character_id.is_none_or(|id| Some(id) == char_id)
        && module_id.is_none_or(|id| id > 0) && effect_id.is_none_or(|id| id > 0)
}
pub(crate) fn validate(event: &GameEvent) -> anyhow::Result<()> {
    let (_, context) = metadata(event).ok_or_else(|| anyhow::anyhow!("missing combat metadata"))?;
    anyhow::ensure!([context.corporation_id, context.alliance_id, context.faction_id, context.ship_id, context.system_id, context.station_id].iter().flatten().all(|&id| id > 0) && context.ship_type_id.is_none_or(|id| id > 0), "invalid combat context");
    match event {
        GameEvent::DamageApplied { char_id, source, target, module_id, weapon_type_id, effect_id, slices, .. } => {
            anyhow::ensure!(source_valid(*char_id, source, *module_id, *effect_id) && weapon_type_id.is_none_or(|v| v > 0), "invalid damage source"); entity(source)?; entity(target)?; slices_valid(slices)?;
        }
        GameEvent::RemoteRepairApplied { char_id, source, target, module_id, effect_id, slices, .. } => {
            anyhow::ensure!(source_valid(*char_id, source, *module_id, *effect_id), "invalid repair source"); entity(source)?; entity(target)?; slices_valid(slices)?;
        }
        GameEvent::WarpScrambleApplied { char_id, source, target, module_id, effect_id, slices, intervals, .. } => {
            anyhow::ensure!(source_valid(*char_id, source, *module_id, *effect_id) && (!slices.is_empty() || !intervals.is_empty()), "invalid scramble source"); entity(source)?; entity(target)?; if !slices.is_empty() { slices_valid(slices)?; }
            for (i, interval) in intervals.iter().enumerate() {
                anyhow::ensure!(interval.active_after > 0 && interval.active_after < interval.active_until && interval.activation.part == SettlementPart::Whole && interval.activation.settlement.target_id == target.item_id && interval.activation.settlement.target_incarnation == target.incarnation, "invalid scramble interval");
                if i > 0 { let p = &intervals[i-1]; anyhow::ensure!(p.active_until <= interval.active_after && (p.activation != interval.activation || p.segment_sequence < interval.segment_sequence), "overlapping scramble intervals"); }
            }
        }
        GameEvent::NpcDestroyed { death_id, npc, system_id, killer_id, killer, context, occurred_at, base_bounty, static_version, .. } => {
            anyhow::ensure!(!death_id.is_empty() && *system_id > 0 && context.system_id.is_none_or(|id| id == *system_id) && *occurred_at > 0 && static_version.as_ref().is_none_or(|s| !s.is_empty() && s.len() <= MAX_STATIC_VERSION_BYTES) && killer_id.is_none_or(|id| id > 0), "invalid npc death");
            entity(npc)?; base_bounty.validate()?;
            match (killer_id, killer) { (Some(id), Some(e)) => { entity(e)?; anyhow::ensure!(e.character_id == Some(*id), "killer mismatch"); }, (None, None) => {}, _ => anyhow::bail!("killer mismatch") }
        }
        GameEvent::LossSettled { char_id, loss_id, killmail_id, victim, occurred_at, final_blow, participants, valuation, .. } => {
            anyhow::ensure!(*char_id > 0 && victim.character_id == Some(*char_id) && *loss_id > 0 && *killmail_id > 0 && *occurred_at > 0 && valuation.valuation_observed_at > 0, "invalid loss"); entity(victim)?; if let Some(e) = final_blow { entity(e)?; }
            let mut credited = std::collections::HashSet::new();
            let mut flagged = false;
            for p in participants {
                entity(&p.entity)?; p.damage.positive()?;
                anyhow::ensure!(p.character_id > 0 && p.entity.character_id.is_none_or(|id| id == p.character_id) && credited.insert(p.character_id), "invalid participant");
                if p.final_blow { anyhow::ensure!(!flagged && final_blow.as_ref() == Some(&p.entity), "invalid final blow"); flagged = true; }
            }
            if !flagged { anyhow::ensure!(final_blow.as_ref().is_none_or(|e| !participants.iter().any(|p| p.entity == *e)), "unflagged credited final blow"); }
            anyhow::ensure!(!valuation.npc_insurance_paid.is_negative(), "negative insurance payment");
            for c in &valuation.components { anyhow::ensure!(c.type_id > 0 && c.quantity > 0, "invalid valuation component"); if let Some(price) = &c.unit_price { price.validate()?; } }
        }
        GameEvent::PlayerKillCredited { char_id, loss_id, killmail_id, attacker, victim, damage, occurred_at, .. } => {
            anyhow::ensure!(*char_id > 0 && attacker.character_id.is_none_or(|id| id == *char_id) && *loss_id > 0 && *killmail_id > 0 && *occurred_at > 0, "invalid kill credit"); entity(attacker)?; entity(victim)?; damage.positive()?;
        }
        _ => anyhow::bail!("not a combat event"),
    }
    Ok(())
}
// Enum strings match the serialized Rust variant names; origin remains lowercase.
macro_rules! lua_enum {
    ($ty:ty { $($variant:ident),+ }) => {
        impl LuaField for $ty {
            fn value(&self, lua: &Lua) -> mlua::Result<Value> {
                Ok(Value::String(lua.create_string(match self { $(Self::$variant => stringify!($variant)),+ })?))
            }
        }
    };
}
lua_enum!(SettlementPart { Whole, Shield, Armor, Hull });
lua_enum!(HpLayer { Shield, Armor, Hull });
lua_enum!(ConflictKind { PlayerVersusPlayer, Npc, Environment, Unknown });
lua_enum!(ValuationCategory { Hull, Fitting, Cargo, DestroyedImplant });
lua_enum!(ValuationSource { RetailPricesAverage });
macro_rules! lua_struct {
    ($ty:ty { $($field:ident),+ }) => {
        impl LuaField for $ty {
            fn value(&self, lua: &Lua) -> mlua::Result<Value> {
                let table = lua.create_table()?;
                $(set_field(lua, &table, stringify!($field), &self.$field)?;)+
                Ok(Value::Table(table))
            }
        }
    };
}
lua_struct!(CombatEntity { item_id, type_id, incarnation, character_id, corporation_id, alliance_id, faction_id });
lua_struct!(SettlementId { source_instance, generation, sequence, target_id, target_incarnation });
lua_struct!(SettlementKey { settlement, part });
lua_struct!(BinaryAmount { coefficient, exponent });
lua_struct!(CombatSlice { index, qualification_epoch, boundary_at, active_until, first_occurred_at, last_occurred_at, successful_count, amount });
lua_struct!(CombatInterval { activation, segment_sequence, active_after, active_until });
lua_struct!(KillParticipant { character_id, entity, final_blow, damage });
lua_struct!(ValuationComponent { category, type_id, quantity, unit_price });
lua_struct!(LossValuation { components, source, source_revision, valuation_observed_at, npc_insurance_paid });
macro_rules! combat_lua {
    ($event:expr, $lua:expr, $table:expr; $($variant:ident { $($field:ident),+ }),+ $(,)?) => {
        match $event {
            $(GameEvent::$variant { $($field),+ } => { $(set_field($lua, $table, stringify!($field), $field)?;)+ },)+
            _ => return Err(mlua::Error::external("not a combat event")),
        }
    };
}
pub(crate) fn encode_lua(event: &GameEvent, lua: &Lua, table: &Table) -> mlua::Result<()> {
    combat_lua!(event, lua, table;
        DamageApplied { char_id, context, source, target, module_id, weapon_type_id, effect_id, layer, slices },
        RemoteRepairApplied { char_id, context, source, target, module_id, effect_id, layer, slices },
        WarpScrambleApplied { char_id, context, source, target, module_id, effect_id, slices, intervals },
        NpcDestroyed { death_id, spawn_generation, npc, system_id, killer_id, killer, context, occurred_at, base_bounty, static_version },
        LossSettled { char_id, loss_id, killmail_id, victim, context, occurred_at, final_blow, participants, conflict, valuation },
        PlayerKillCredited { char_id, loss_id, killmail_id, attacker, victim, final_blow, damage, context, occurred_at },
    );
    Ok(())
}

#[cfg(test)] mod tests;
