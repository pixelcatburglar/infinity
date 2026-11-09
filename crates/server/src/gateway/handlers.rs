//! Request routing for requestBroker.py and its feature messengers.
//! Uses descriptors, CDN data and DAO-backed handlers; unknown requests retain
//! the existing fallback behavior and do not claim future feature methods.
use prost_reflect::DynamicMessage;
use super::{context::HandlerContext, achievements, character_titles, space_content, career_goals, insurgency, characters, plex, skill_plans, ship_cosmetics, error::GatewayError, message::{message, response_name}};

#[derive(Clone, Copy)]
enum Route {
    AchievementDefinition,
    CharacterTitleDefinition,
    SpaceContent,
    CareerGoalDefinitions,
    CareerGoalStates,
    Corruption,
    Suppression,
    CharacterNamesByUser,
    PlexBalance,
    PersonalSkillPlan,
    PersonalMilestone,
    ShipCosmetics,
}

const CLAIMS: &[(&str, Route)] = &[
    ("eve_public.achievement.definition.api.cdn.GetLatestRequest", Route::AchievementDefinition),
    ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Route::CharacterTitleDefinition),
    ("eve_public.space.api.cdn.GetUpdateRequest", Route::SpaceContent),
    ("eve_public.career.goal.api.GetDefinitionsRequest", Route::CareerGoalDefinitions),
    ("eve_public.career.goal.api.GetAllRequest", Route::CareerGoalStates),
    ("eve_public.pirate.corruption.api.GetStageThresholdsRequest", Route::Corruption),
    ("eve_public.pirate.suppression.api.GetStageThresholdsRequest", Route::Suppression),
    ("eve_public.character.GetNamesByUserRequest", Route::CharacterNamesByUser),
    ("eve_public.plex.vault.api.BalanceRequest", Route::PlexBalance),
    ("eve_public.cosmetic.ship.api.GetRequest", Route::ShipCosmetics),
    ("eve_public.cosmetic.ship.api.GetAllInBubbleRequest", Route::ShipCosmetics),
    ("eve_public.character.skill.plan.CreateRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.GetAllRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.GetRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.GetSharedRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.GetActiveRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.SetActiveRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.DeleteRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.SetNameRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.SetDescriptionRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.SetSkillRequirementsRequest", Route::PersonalSkillPlan),
    ("eve_public.character.skill.plan.milestone.GetAllRequest", Route::PersonalMilestone),
    ("eve_public.character.skill.plan.milestone.CreateRequest", Route::PersonalMilestone),
    ("eve_public.character.skill.plan.milestone.DeleteRequest", Route::PersonalMilestone),
    ("eve_public.character.skill.plan.milestone.SetDescriptionRequest", Route::PersonalMilestone),
];

pub(crate) fn is_implemented(name: &str) -> bool {
    CLAIMS.iter().any(|(claim, _)| *claim == name)
}

pub(super) fn validate() -> anyhow::Result<()> {
    for &(name, _) in CLAIMS {
        anyhow::ensure!(message(name).is_some(), "missing request descriptor {name}");
        let response = response_name(name).expect("claimed request name");
        anyhow::ensure!(message(&response).is_some(), "missing response descriptor {response}");
    }
    ship_cosmetics::validate()?;
    crate::services::daily_goals::wire::validate()?;
    Ok(())
}

pub(super) enum Dispatch { Unclaimed, Claimed(Result<DynamicMessage, GatewayError>) }

pub(super) async fn handle(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> Dispatch {
    let Some((_, route)) = CLAIMS.iter().find(|(claim, _)| *claim == name) else { return Dispatch::Unclaimed };
    let result = match route {
        Route::AchievementDefinition => achievements::definition::handle(ctx, name).map_err(GatewayError::Internal),
        Route::CharacterTitleDefinition => character_titles::definition::handle(ctx, name).map_err(GatewayError::Internal),
        Route::SpaceContent => space_content::handle(ctx, name).map_err(GatewayError::Internal),
        Route::CareerGoalDefinitions => career_goals::career_goal_definitions().map_err(GatewayError::Internal),
        Route::CareerGoalStates => career_goals::career_goal_states().map_err(GatewayError::Internal),
        Route::Corruption => insurgency::corruption::handle(name).map_err(GatewayError::Internal),
        Route::Suppression => insurgency::suppression::handle(name).map_err(GatewayError::Internal),
        Route::CharacterNamesByUser => characters::names_by_user(ctx).await,
        Route::PlexBalance => plex::plex_balance(ctx).await,
        Route::PersonalSkillPlan => skill_plans::personal::skill_plan(ctx, name, req).await,
        Route::PersonalMilestone => skill_plans::personal_milestones::handle(ctx, name, req).await,
        Route::ShipCosmetics => ship_cosmetics::handle(ctx, name, req).await,
    };
    Dispatch::Claimed(result)
}

#[cfg(test)] mod tests;
