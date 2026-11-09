//! Canonical facts queued after guarded daily completion and reward writes.
//! The caller owns the transaction and must not invoke these on replay.
use infinity_core::consts::CORP_HERALDRY;
use crate::{dao::daily_goals::{instances::GoalRow, rewards::RewardRow}, db::outbox::{Outbox, SourceKey}, lua::{content::daily_goals::model::Category, events::{GameEvent, goals::{CompletionReason, EverMarkReason, GoalCategory, GoalPeriod}, resources::{FactContext, FactOrigin}}}};
use super::{Error, GoalSnapshot, ids};

fn context(origin: FactOrigin) -> FactContext {
    FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin }
}

/// Queue only after the first guarded transition to completion succeeds.
pub(crate) fn completed(outbox: &mut Outbox, goal: &GoalRow, snapshot: &GoalSnapshot, reason: CompletionReason, origin: FactOrigin, at: i64) -> Result<(), Error> {
    let category = match snapshot.template.category { Category::Ordinary => GoalCategory::Ordinary, Category::Bonus => GoalCategory::Bonus, Category::Month => GoalCategory::Month };
    let (category_text, period) = match category { GoalCategory::Ordinary => ("ordinary", GoalPeriod::Day), GoalCategory::Bonus => ("bonus", GoalPeriod::Day), GoalCategory::Month => ("month", GoalPeriod::Month) };
    let period_text = match period { GoalPeriod::Day => "day", GoalPeriod::Month => "month" };
    let stored: GoalSnapshot = serde_json::from_str(&goal.definition).map_err(|_| Error::Invalid)?;
    let event = GameEvent::DailyGoalCompleted { char_id: goal.character_id, goal_id: goal.goal_id.clone(), category, period, period_start: goal.period_start, period_end: goal.period_end, paid: goal.paid, reason, occurred_at: at, context: context(origin) };
    if goal.goal_id != ids::goal(goal.character_id, goal.period_start, &goal.version, &goal.slot_key, goal.generation) || serde_json::to_value(&stored).map_err(|_| Error::Invalid)? != serde_json::to_value(snapshot).map_err(|_| Error::Invalid)? || goal.category != category_text || goal.kind != period_text || goal.target == 0 || goal.target != snapshot.template.target || goal.progress != goal.target || goal.completed_at != Some(at) || goal.voided_at.is_some() { return Err(Error::Invalid); }
    crate::lua::events::goals::validate(&event).map_err(|_| Error::Invalid)?;
    outbox.event_from(SourceKey::canonical("DailyGoalCompleted", &goal.goal_id, 0), event);
    Ok(())
}

/// Queue only after the LP credit and guarded reward receipt both succeed.
pub(crate) fn evermarks(outbox: &mut Outbox, goal: &GoalRow, reward: &RewardRow, at: i64) -> Result<(), Error> {
    let uuid = crate::uuid_text::parse(&goal.goal_id, crate::uuid_text::Case::Lowercase).ok_or(Error::Invalid)?;
    let amount = reward.amount_units.and_then(|n| i64::try_from(n).ok()).ok_or(Error::Invalid)?;
    if reward.goal_id != goal.goal_id || goal.goal_id != ids::goal(goal.character_id, goal.period_start, &goal.version, &goal.slot_key, goal.generation) || goal.completed_at.is_none() || goal.voided_at.is_some() || reward.unit != "lp" || reward.issuer_id != Some(CORP_HERALDRY) || reward.amount_isk.is_some() || reward.type_id.is_some() || !reward.earned || reward.claimed_at != Some(at) || reward.asset_id != ids::asset(goal.character_id, goal.period_start, &goal.version, &goal.slot_key, goal.generation, &uuid, reward.reward_index) { return Err(Error::Invalid); }
    let event = GameEvent::EverMarksEarned { char_id: goal.character_id, goal_id: goal.goal_id.clone(), reward_index: reward.reward_index, asset_id: reward.asset_id.clone(), issuer_id: CORP_HERALDRY, amount, reason: EverMarkReason::DailyReward, occurred_at: at, context: context(FactOrigin::Natural) };
    crate::lua::events::goals::validate(&event).map_err(|_| Error::Invalid)?;
    outbox.event_from(SourceKey::canonical("EverMarksEarned", format!("{}:{}:{}", goal.goal_id, reward.reward_index, reward.asset_id), 0), event);
    Ok(())
}
#[cfg(test)] mod tests;
