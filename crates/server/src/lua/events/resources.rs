//! Historical resource facts and their Lua table representation.
//! These server-side snapshots are not client protocol objects and retain no live handles.

use mlua::{Lua, Table};
use super::{GameEvent, fields::{LuaField, set_field}};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FactOrigin { Natural, Gm, Qa }
impl FactOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Natural => "natural", Self::Gm => "gm", Self::Qa => "qa" }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FactContext {
    pub corporation_id: Option<i64>, pub alliance_id: Option<i64>, pub faction_id: Option<i64>,
    pub ship_id: Option<i64>, pub ship_type_id: Option<i32>, pub system_id: Option<i64>,
    pub station_id: Option<i64>, pub origin: FactOrigin,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MiningSlice { pub log_id: i64, pub occurred_at: i64, pub quantity: i64, pub volume_m3: f64 }

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MiningResourceKind { Ore, Gas }
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HackKind { Data, Relic }
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoyaltyReason { MissionReward, GmAdjustment, StorePurchase, ChipExchange, DailyReward, DailyCompletion }

impl GameEvent {
    pub(crate) fn resource_metadata(&self) -> Option<(i64, &FactContext)> {
        match self {
            Self::OreMined { context, slices, .. } | Self::GasHarvested { context, slices, .. } =>
                slices.iter().map(|slice| slice.occurred_at).min().map(|time| (time, context)),
            Self::ManufacturingInstalled { occurred_at, context, .. }
            | Self::ManufacturingDelivered { occurred_at, context, .. }
            | Self::SignatureResolved { occurred_at, context, .. }
            | Self::WreckSalvaged { occurred_at, context, .. }
            | Self::ContainerHacked { occurred_at, context, .. }
            | Self::LoyaltyPointsEarned { occurred_at, context, .. } => Some((*occurred_at, context)),
            _ => None,
        }
    }
}

macro_rules! resource_events {
    ($($variant:ident { $($field:ident),* }),* $(,)?) => {
        pub(super) fn known_kind(kind: &str) -> bool { matches!(kind, $(stringify!($variant))|*) }
        pub(super) fn kind(event: &GameEvent) -> &'static str {
            match event { $(GameEvent::$variant { .. } => stringify!($variant),)* _ => unreachable!() }
        }
        pub(super) fn encode_lua(event: &GameEvent, lua: &Lua, table: &Table) -> mlua::Result<()> {
            match event { $(GameEvent::$variant { $($field),* } => { $(set_field(lua, table, stringify!($field), $field)?;)* },)* _ => unreachable!() }
            Ok(())
        }
    };
}

impl LuaField for MiningSlice {
    fn value(&self, lua: &Lua) -> mlua::Result<mlua::Value> {
        let row = lua.create_table()?;
        row.set("log_id", self.log_id)?; row.set("occurred_at", self.occurred_at)?;
        row.set("quantity", self.quantity)?; row.set("volume_m3", self.volume_m3)?;
        Ok(mlua::Value::Table(row))
    }
}
impl LuaField for HackKind {
    fn value(&self, lua: &Lua) -> mlua::Result<mlua::Value> {
        Ok(mlua::Value::String(lua.create_string(match self { Self::Data => "data", Self::Relic => "relic" })?))
    }
}
impl LuaField for LoyaltyReason {
    fn value(&self, lua: &Lua) -> mlua::Result<mlua::Value> {
        Ok(mlua::Value::String(lua.create_string(match self { Self::MissionReward => "mission_reward", Self::GmAdjustment => "gm_adjustment", Self::StorePurchase => "store_purchase", Self::ChipExchange => "chip_exchange", Self::DailyReward => "daily_reward", Self::DailyCompletion => "daily_completion" })?))
    }
}
resource_events! {
    OreMined { char_id, ore_type, context, slices },
    GasHarvested { char_id, gas_type, context, slices },
    ManufacturingInstalled { char_id, job_id, owner_id, owner_is_corporation, product_type, runs, scheduled_quantity, facility_id, occurred_at, context },
    ManufacturingDelivered { char_id, installer_id, job_id, owner_id, owner_is_corporation, product_type, quantity, facility_id, output_location_id, occurred_at, context },
    SignatureResolved { char_id, site_id, signature_id, signature_type, archetype, dungeon_type, occurred_at, context },
    WreckSalvaged { char_id, wreck_id, wreck_type, occurred_at, context },
    ContainerHacked { char_id, container_id, container_type, site_id, archetype, hack_kind, occurred_at, context },
    LoyaltyPointsEarned { char_id, issuer_id, amount, reason, occurred_at, context },
}

#[cfg(test)] mod tests;
