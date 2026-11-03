//! Typed game events delivered after successful operations. Payloads contain only
//! scalar values, not client context or database handles; delivery is asynchronous.
//! These events describe server operations rather than a client-side API.

use mlua::{Lua, Table};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum GameEvent {
    CharacterSelected { user_id: i64, char_id: i64 },
    CharacterLoggedOff { user_id: i64, char_id: i64 },
    Undocked { char_id: i64, ship_id: i64, station_id: i64, system_id: i64 },
    Docked { char_id: i64, ship_id: i64, station_id: i64, system_id: i64 },
    Jumped { char_id: i64, ship_id: i64, from_system_id: i64, to_system_id: i64 },
    ShipLost { char_id: i64, ship_id: i64, type_id: i32, system_id: i64 },
    NpcKilled { char_id: i64, npc_id: i64, type_id: i32, system_id: i64 },
    ItemsMoved { char_id: i64, item_id: i64, from_location_id: i64, to_location_id: i64, quantity: i32 },
    WalletChanged { owner_id: i64, account_key: i32, balance: f64, delta: f64 },
    SkillTrained { char_id: i64, type_id: i32, level: i32 },
    MarketOrderPlaced { char_id: i64, order_id: i64, type_id: i32, quantity: i32, price: f64, is_buy: bool },
    MarketOrderFilled { order_id: i64, buyer_id: i64, seller_id: i64, type_id: i32, quantity: i32, price: f64 },
    MissionAccepted { char_id: i64, agent_id: i64, mission_id: i32 },
    MissionCompleted { char_id: i64, agent_id: i64, mission_id: i32 },
    BallparkBound { char_id: i64, system_id: i64 },
    Targeted { char_id: i64, by_ball_id: i64, system_id: i64 },
    Damaged { char_id: i64, ship_id: i64, attacker_ball_id: i64, system_id: i64 },
    ModuleCycleEnded { char_id: i64, module_item_id: i64, effect_id: i64, outcome: ModuleCycleOutcome },
    WarpArrived { char_id: i64, system_id: i64 },
}

macro_rules! events {
    ($($variant:ident { $($field:ident),* }),* $(,)?) => {
        impl GameEvent {
            pub fn kind(&self) -> &'static str {
                match self { $(Self::$variant { .. } => stringify!($variant)),* }
            }

            pub fn to_lua(&self, lua: &Lua) -> mlua::Result<Table> {
                let table = lua.create_table()?;
                table.set("kind", self.kind())?;
                match self { $(Self::$variant { $($field),* } => { $(table.set(stringify!($field), *$field)?;)* }),* }
                Ok(table)
            }
        }

        pub(super) fn known_kind(kind: &str) -> bool {
            matches!(kind, $(stringify!($variant))|*)
        }
    };
}

events! {
    CharacterSelected { user_id, char_id }, CharacterLoggedOff { user_id, char_id },
    Undocked { char_id, ship_id, station_id, system_id }, Docked { char_id, ship_id, station_id, system_id },
    Jumped { char_id, ship_id, from_system_id, to_system_id }, ShipLost { char_id, ship_id, type_id, system_id },
    NpcKilled { char_id, npc_id, type_id, system_id }, ItemsMoved { char_id, item_id, from_location_id, to_location_id, quantity },
    WalletChanged { owner_id, account_key, balance, delta }, SkillTrained { char_id, type_id, level },
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
