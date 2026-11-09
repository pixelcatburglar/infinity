//! Frozen daily-goal definitions and reward assets.
//! Client contribution metadata comes from the native SDE; definitions come from retained content.
//! Settlement and mutable reward state are not handled here.
use infinity_core::consts::CORP_HERALDRY;
use crate::{dao::daily_goals::{instances::GoalRow, rewards::RewardRow}, lua::content::daily_goals::model::{Reward, Template}, sde::Sde};
use super::{Error, GoalSnapshot, MessageRefs, ids};

pub(crate) fn snapshot(sde: &Sde, template: &Template) -> Result<GoalSnapshot, Error> {
    let metadata = sde.contribution_methods.get(template.method.metadata_key()).ok_or(Error::Unavailable)?;
    let name = u64::from(u32::try_from(metadata.title_id).map_err(|_| Error::Unavailable)?);
    let description = u64::from(u32::try_from(metadata.description_id).map_err(|_| Error::Unavailable)?);
    if name == 0 || description == 0 { return Err(Error::Unavailable); }
    Ok(GoalSnapshot { template: template.clone(), messages: MessageRefs { name, description, help: description }, assigner: CORP_HERALDRY })
}

pub(crate) fn rewards(goal: &GoalRow, template: &Template) -> Result<Vec<RewardRow>, Error> {
    let uuid = crate::uuid_text::parse(&goal.goal_id, crate::uuid_text::Case::Lowercase).ok_or(Error::Invalid)?;
    template.rewards.iter().enumerate().map(|(index, reward)| {
        let index = i32::try_from(index).map_err(|_| Error::Invalid)?;
        let (unit, amount_isk, amount_units, issuer_id, type_id) = match reward {
            Reward::Isk { amount } => ("isk", Some(*amount), None, None, None),
            Reward::Evermark { amount } => ("lp", None, Some(*amount), Some(CORP_HERALDRY), None),
            Reward::SkillPoints { amount } => ("sp", None, Some(u64::from(*amount)), None, None),
            Reward::Item { type_id, amount } => ("item", None, Some(u64::from(*amount)), None, Some(*type_id)),
            Reward::Plex { whole_units } => ("plex", None, Some(*whole_units), None, None),
        };
        Ok(RewardRow { goal_id: goal.goal_id.clone(), reward_index: index, asset_id: ids::asset(goal.character_id, goal.period_start, &goal.version, &goal.slot_key, goal.generation, &uuid, index), unit: unit.into(), amount_isk, amount_units, issuer_id, type_id, omega_required: template.omega_required, earned: false, claimed_at: None, location_id: None, item_id: None, plex_reference: None, revision: 0 })
    }).collect()
}

pub(crate) fn validate(goal: &GoalRow) -> Result<GoalSnapshot, Error> {
    serde_json::from_str(&goal.definition)
        .map_err(|_| Error::Internal(crate::services::db_err(anyhow::anyhow!("invalid frozen daily goal"))))
}
