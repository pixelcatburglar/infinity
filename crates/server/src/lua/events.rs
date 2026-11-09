//! Typed game events delivered after successful operations, including historical
//! resource snapshots. These describe server operations rather than a client API;
//! they contain no live client context or database handles.

use mlua::{Lua, Table};

pub(crate) mod resources;
pub(crate) mod combat;
pub(crate) mod goals;
mod fields;
use combat::{BinaryAmount, CombatEntity, CombatInterval, CombatSlice, ConflictKind, HpLayer, KillParticipant, LossValuation};
use resources::{FactContext, HackKind, LoyaltyReason, MiningSlice};

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModuleCycleOutcome {
    Completed,
    TargetLost,
    OutOfCapacitor,
    CargoFull,
}

impl mlua::IntoLua for ModuleCycleOutcome {
    fn into_lua(self, lua: &Lua) -> mlua::Result<mlua::Value> {
        let value = match self {
            Self::Completed => "completed",
            Self::TargetLost => "target_lost",
            Self::OutOfCapacitor => "out_of_capacitor",
            Self::CargoFull => "cargo_full",
        };
        Ok(mlua::Value::String(lua.create_string(value)?))
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum GameEvent {
    CharacterSelected { user_id: i64, char_id: i64 },
    CharacterLoggedOff { user_id: i64, char_id: i64 },
    Undocked { char_id: i64, ship_id: i64, station_id: i64, system_id: i64 },
    Docked { char_id: i64, ship_id: i64, station_id: i64, system_id: i64 },
    Jumped { char_id: i64, ship_id: i64, from_system_id: i64, to_system_id: i64 },
    ShipLost { char_id: i64, ship_id: i64, type_id: i32, system_id: i64 },
    NpcKilled { char_id: i64, npc_id: i64, type_id: i32, system_id: i64 },
    ItemsMoved { char_id: i64, item_id: i64, from_location_id: i64, to_location_id: i64, quantity: i64 },
    WalletChanged { owner_id: i64, account_key: i32, balance: crate::isk::IskAmount, delta: crate::isk::IskAmount },
    SkillTrained { char_id: i64, type_id: i32, level: i32 },
    SkillStateChanged { char_id: i64 },
    MarketOrderPlaced { char_id: i64, order_id: i64, type_id: i32, quantity: i64, price: crate::isk::IskAmount, is_buy: bool },
    MarketOrderFilled { order_id: i64, buyer_id: i64, seller_id: i64, type_id: i32, quantity: i64, price: crate::isk::IskAmount },
    MissionAccepted { char_id: i64, agent_id: i64, mission_id: i32 },
    MissionCompleted { char_id: i64, agent_id: i64, mission_id: i32 },
    BallparkBound { char_id: i64, system_id: i64 },
    Targeted { char_id: i64, by_ball_id: i64, system_id: i64 },
    Damaged { char_id: i64, ship_id: i64, attacker_ball_id: i64, system_id: i64 },
    ModuleCycleEnded { char_id: i64, module_item_id: i64, effect_id: i64, outcome: ModuleCycleOutcome },
    WarpArrived { char_id: i64, system_id: i64 },
    OreMined { char_id: i64, ore_type: i32, context: FactContext, slices: Vec<MiningSlice> },
    GasHarvested { char_id: i64, gas_type: i32, context: FactContext, slices: Vec<MiningSlice> },
    ManufacturingInstalled { char_id: i64, job_id: i64, owner_id: i64, owner_is_corporation: bool, product_type: i32, runs: i32, scheduled_quantity: i64, facility_id: i64, occurred_at: i64, context: FactContext },
    ManufacturingDelivered { char_id: i64, installer_id: i64, job_id: i64, owner_id: i64, owner_is_corporation: bool, product_type: i32, quantity: i64, facility_id: i64, output_location_id: i64, occurred_at: i64, context: FactContext },
    SignatureResolved { char_id: i64, site_id: i64, signature_id: String, signature_type: i64, archetype: Option<i64>, dungeon_type: Option<i64>, occurred_at: i64, context: FactContext },
    WreckSalvaged { char_id: i64, wreck_id: i64, wreck_type: i32, occurred_at: i64, context: FactContext },
    ContainerHacked { char_id: i64, container_id: i64, container_type: i32, site_id: i64, archetype: Option<i64>, hack_kind: HackKind, occurred_at: i64, context: FactContext },
    LoyaltyPointsEarned { char_id: i64, issuer_id: i64, amount: i64, reason: LoyaltyReason, occurred_at: i64, context: FactContext },
    DamageApplied { char_id: Option<i64>, context: FactContext, source: CombatEntity, target: CombatEntity, module_id: Option<i64>, weapon_type_id: Option<i32>, effect_id: Option<i64>, layer: HpLayer, slices: Vec<CombatSlice> },
    RemoteRepairApplied { char_id: Option<i64>, context: FactContext, source: CombatEntity, target: CombatEntity, module_id: Option<i64>, effect_id: Option<i64>, layer: HpLayer, slices: Vec<CombatSlice> },
    WarpScrambleApplied { char_id: Option<i64>, context: FactContext, source: CombatEntity, target: CombatEntity, module_id: Option<i64>, effect_id: Option<i64>, slices: Vec<CombatSlice>, intervals: Vec<CombatInterval> },
    NpcDestroyed { death_id: String, spawn_generation: u64, npc: CombatEntity, system_id: i64, killer_id: Option<i64>, killer: Option<CombatEntity>, context: FactContext, occurred_at: i64, base_bounty: BinaryAmount, static_version: Option<String> },
    LossSettled { char_id: i64, loss_id: i64, killmail_id: i64, victim: CombatEntity, context: FactContext, occurred_at: i64, final_blow: Option<CombatEntity>, participants: Vec<KillParticipant>, conflict: ConflictKind, valuation: LossValuation },
    PlayerKillCredited { char_id: i64, loss_id: i64, killmail_id: i64, attacker: CombatEntity, victim: CombatEntity, final_blow: bool, damage: BinaryAmount, context: FactContext, occurred_at: i64 },
    DailyGoalCompleted { char_id: i64, goal_id: String, category: goals::GoalCategory, period: goals::GoalPeriod, period_start: i64, period_end: i64, paid: bool, reason: goals::CompletionReason, occurred_at: i64, context: FactContext },
    EverMarksEarned { char_id: i64, goal_id: String, reward_index: i32, asset_id: String, issuer_id: i64, amount: i64, reason: goals::EverMarkReason, occurred_at: i64, context: FactContext },
}

macro_rules! events {
    ($($variant:ident { $($field:ident),* }),* $(,)?) => {
        impl GameEvent {
            pub fn kind(&self) -> &'static str {
                match self { $(Self::$variant { .. } => stringify!($variant),)* other => combat::kind(other).or_else(|| goals::kind(other)).unwrap_or_else(|| resources::kind(other)) }
            }

            pub fn to_lua(&self, lua: &Lua) -> mlua::Result<Table> {
                let table = lua.create_table()?;
                table.set("kind", self.kind())?;
                match self { $(Self::$variant { $($field),* } => { $(table.set(stringify!($field), *$field)?;)* },)* other if combat::kind(other).is_some() => combat::encode_lua(other, lua, &table)?, other if goals::kind(other).is_some() => goals::encode_lua(other, lua, &table)?, other => resources::encode_lua(other, lua, &table)? }
                Ok(table)
            }
        }

        pub(super) fn known_kind(kind: &str) -> bool {
            matches!(kind, $(stringify!($variant))|*) || resources::known_kind(kind) || combat::known_kind(kind) || goals::known_kind(kind)
        }
    };
}

events! {
    CharacterSelected { user_id, char_id }, CharacterLoggedOff { user_id, char_id },
    Undocked { char_id, ship_id, station_id, system_id }, Docked { char_id, ship_id, station_id, system_id },
    Jumped { char_id, ship_id, from_system_id, to_system_id }, ShipLost { char_id, ship_id, type_id, system_id },
    NpcKilled { char_id, npc_id, type_id, system_id }, ItemsMoved { char_id, item_id, from_location_id, to_location_id, quantity },
    WalletChanged { owner_id, account_key, balance, delta }, SkillTrained { char_id, type_id, level },
    SkillStateChanged { char_id },
    MarketOrderPlaced { char_id, order_id, type_id, quantity, price, is_buy },
    MarketOrderFilled { order_id, buyer_id, seller_id, type_id, quantity, price },
    MissionAccepted { char_id, agent_id, mission_id }, MissionCompleted { char_id, agent_id, mission_id },
    BallparkBound { char_id, system_id },
    Targeted { char_id, by_ball_id, system_id },
    Damaged { char_id, ship_id, attacker_ball_id, system_id },
    ModuleCycleEnded { char_id, module_item_id, effect_id, outcome },
    WarpArrived { char_id, system_id },
}

#[cfg(test)]
mod tests;
