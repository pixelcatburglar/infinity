//! Route requests from `requestBroker.py:_send_request` to gateway handlers.
//! Handlers use descriptors, CDN content and DAO data, not World; unclaimed
//! requests fall through to the existing no-content or default response.
use anyhow::Result;
use prost_reflect::DynamicMessage;
use super::{Gateway, Caller, achievements, character_titles, space_content, career_goals, insurgency, characters, plex, skill_plans};

pub(super) async fn handle(gw: &Gateway, caller: &Caller, name: &str, req: &DynamicMessage) -> Result<Option<DynamicMessage>> {
    match name {
        "eve_public.achievement.definition.api.cdn.GetLatestRequest" => achievements::definition::handle(gw, name).map(Some),
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest" => character_titles::definition::handle(gw, name).map(Some),
        "eve_public.space.api.cdn.GetUpdateRequest" => space_content::handle(gw, name).map(Some),
        "eve_public.career.goal.api.GetDefinitionsRequest" => career_goals::career_goal_definitions().map(Some),
        "eve_public.career.goal.api.GetAllRequest" => career_goals::career_goal_states().map(Some),
        "eve_public.pirate.corruption.api.GetStageThresholdsRequest" => insurgency::corruption::handle(name).map(Some),
        "eve_public.pirate.suppression.api.GetStageThresholdsRequest" => insurgency::suppression::handle(name).map(Some),
        "eve_public.character.GetNamesByUserRequest" => characters::names_by_user(gw, caller).await.map(Some),
        "eve_public.plex.vault.api.BalanceRequest" => plex::plex_balance(gw, caller).await.map(Some),
        n if n.starts_with("eve_public.character.skill.plan.") && !n.contains(".milestone.") => skill_plans::personal::skill_plan(gw, caller, n, req).await,
        _ => Ok(None),
    }
}
