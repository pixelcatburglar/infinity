//! Immutable daily-goal completion and EverMark reward facts.
//! These server events retain historical scalar data, not client protocol objects.
use mlua::{Lua, Table};
use serde::{Deserialize, Serialize};
use super::{GameEvent, fields::{LuaField, set_field}, resources::{FactContext, FactOrigin}};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalCategory { Ordinary, Bonus, Month }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalPeriod { Day, Month }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompletionReason { Natural, Paid, Qa, Derived }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EverMarkReason { DailyReward }

pub(crate) fn kind(event: &GameEvent) -> Option<&'static str> {
    match event { GameEvent::DailyGoalCompleted { .. } => Some("DailyGoalCompleted"), GameEvent::EverMarksEarned { .. } => Some("EverMarksEarned"), _ => None }
}
pub(crate) fn known_kind(name: &str) -> bool { matches!(name, "DailyGoalCompleted" | "EverMarksEarned") }
pub(crate) fn metadata(event: &GameEvent) -> Option<(i64, &FactContext)> {
    match event { GameEvent::DailyGoalCompleted { occurred_at, context, .. } | GameEvent::EverMarksEarned { occurred_at, context, .. } => Some((*occurred_at, context)), _ => None }
}
pub(crate) fn validate(event: &GameEvent) -> anyhow::Result<()> {
    let (time, context) = metadata(event).ok_or_else(|| anyhow::anyhow!("not a daily goal event"))?;
    anyhow::ensure!(time > 0 && [context.corporation_id, context.alliance_id, context.faction_id, context.ship_id, context.system_id, context.station_id].iter().flatten().all(|id| *id > 0) && context.ship_type_id.is_none_or(|id| id > 0), "invalid daily fact context");
    let uuid = |s: &str| crate::uuid_text::parse(s, crate::uuid_text::Case::Lowercase).is_some();
    match event {
        GameEvent::DailyGoalCompleted { char_id, goal_id, category, period, period_start, period_end, paid, reason, .. } => {
            let origin_ok = match reason { CompletionReason::Natural | CompletionReason::Paid => context.origin == FactOrigin::Natural, CompletionReason::Qa => context.origin == FactOrigin::Qa, CompletionReason::Derived => matches!(context.origin, FactOrigin::Natural | FactOrigin::Qa) };
            anyhow::ensure!(*char_id > 0 && uuid(goal_id) && *period_start >= 0 && period_start < period_end && matches!((category, period), (GoalCategory::Ordinary | GoalCategory::Bonus, GoalPeriod::Day) | (GoalCategory::Month, GoalPeriod::Month)) && *paid == (*reason == CompletionReason::Paid) && origin_ok, "invalid daily completion");
        }
        GameEvent::EverMarksEarned { char_id, goal_id, reward_index, asset_id, issuer_id, amount, reason, .. } => {
            anyhow::ensure!(*char_id > 0 && uuid(goal_id) && *reward_index >= 0 && uuid(asset_id) && *issuer_id == infinity_core::consts::CORP_HERALDRY && *amount > 0 && *reason == EverMarkReason::DailyReward && context.origin == FactOrigin::Natural, "invalid EverMark reward");
        }
        _ => unreachable!(),
    }
    Ok(())
}
// Lua labels are the serialized enum variant names.
macro_rules! lua_enum { ($ty:ty { $($v:ident),+ }) => { impl LuaField for $ty { fn value(&self, lua: &Lua) -> mlua::Result<mlua::Value> { Ok(mlua::Value::String(lua.create_string(match self { $(Self::$v => stringify!($v)),+ })?)) } } }; }
lua_enum!(GoalCategory { Ordinary, Bonus, Month });
lua_enum!(GoalPeriod { Day, Month });
lua_enum!(CompletionReason { Natural, Paid, Qa, Derived });
lua_enum!(EverMarkReason { DailyReward });
pub(crate) fn encode_lua(event: &GameEvent, lua: &Lua, table: &Table) -> mlua::Result<()> {
    macro_rules! fields { ($($field:ident),+) => {{ $(set_field(lua, table, stringify!($field), $field)?;)+ }}; }
    match event {
        GameEvent::DailyGoalCompleted { char_id, goal_id, category, period, period_start, period_end, paid, reason, occurred_at, context } => fields!(char_id, goal_id, category, period, period_start, period_end, paid, reason, occurred_at, context),
        GameEvent::EverMarksEarned { char_id, goal_id, reward_index, asset_id, issuer_id, amount, reason, occurred_at, context } => fields!(char_id, goal_id, reward_index, asset_id, issuer_id, amount, reason, occurred_at, context),
        _ => return Err(mlua::Error::external("not a daily goal event")),
    }
    Ok(())
}
#[cfg(test)] mod tests;
