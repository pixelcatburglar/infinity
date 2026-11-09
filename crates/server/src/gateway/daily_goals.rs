//! Daily-goal namespace reservation and scoped Lua-to-native gateway binding.
//! Uses the public descriptors and retained daily-goal service kernels; retired
//! and unknown methods in this namespace never reach the default response path.

use std::sync::Arc;
use prost_reflect::DynamicMessage;
use super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{message, msg_field, response_name}};
use infinity_core::{consts::UUID_LEN, messages::gateway as text};
use crate::{lua::gateway::{self, GatewayInvocationIdentity, GatewayMode, GatewayOperation}, services::daily_goals::{self as daily, Error, Resources}, space::World};

pub(crate) const ADMIN_PROGRESS: &str = "eve_public.dailygoal.api.admin.ProgressRequest";
const SUPPORTED: &[&str] = &[
    "eve_public.dailygoal.api.GetAllCurrentRequest",
    "eve_public.dailygoal.api.GetRequest",
    "eve_public.dailygoal.api.GetAllWithRewardsRequest",
    "eve_public.dailygoal.api.RedeemRequest",
    "eve_public.dailygoal.api.PayForCompletionRequest",
    ADMIN_PROGRESS,
];

pub(crate) fn supported(name: &str) -> bool { SUPPORTED.contains(&name) }
pub(super) fn owns(name: &str) -> bool { name.starts_with("eve_public.dailygoal.api.") }

pub(super) fn validate() -> anyhow::Result<()> {
    for &name in SUPPORTED {
        anyhow::ensure!(message(name).is_some(), "missing request descriptor {name}");
        let response = response_name(name).expect("supported request name");
        anyhow::ensure!(message(&response).is_some(), "missing response descriptor {response}");
    }
    Ok(())
}

pub(super) fn unavailable(name: &str, correlation_uuid: &[u8]) -> GatewayError {
    if name == ADMIN_PROGRESS && correlation_uuid.len() != UUID_LEN {
        GatewayError::refusal(FailureCode::BadRequest, text::INVALID_REQUEST)
    } else if supported(name) {
        GatewayError::Internal(anyhow::anyhow!("daily goal gateway unavailable"))
    } else {
        GatewayError::refusal(FailureCode::NotImplemented, text::NOT_IMPLEMENTED)
    }
}

fn error(error: Error) -> GatewayError {
    match error {
        Error::Invalid => GatewayError::refusal(FailureCode::BadRequest, text::INVALID_REQUEST),
        Error::Unauthenticated => GatewayError::refusal(FailureCode::Unauthorized, text::UNAUTHORIZED),
        Error::Missing => GatewayError::refusal(FailureCode::NotFound, text::NOT_FOUND),
        Error::Forbidden => GatewayError::refusal(FailureCode::Forbidden, text::FORBIDDEN),
        Error::Unavailable => GatewayError::Internal(anyhow::anyhow!("daily goals unavailable")),
        Error::Internal(error) => GatewayError::Internal(anyhow::anyhow!("daily goals: {error:?}")),
    }
}

fn invalid() -> GatewayError { GatewayError::refusal(FailureCode::BadRequest, text::INVALID_REQUEST) }

fn goal_id(req: &DynamicMessage) -> Result<String, GatewayError> {
    if !req.has_field_by_name("goal") { return Err(invalid()); }
    let goal = msg_field(req, "goal").ok_or_else(invalid)?;
    let value = goal.get_field_by_name("uuid").ok_or_else(invalid)?;
    let bytes: &[u8] = value.as_bytes().ok_or_else(invalid)?;
    let uuid: &[u8; UUID_LEN] = bytes.try_into().map_err(|_| invalid())?;
    Ok(crate::uuid_text::format(uuid))
}

enum Operation {
    Current,
    Get(String),
    History(u32, String),
    Redeem(String, bool),
    Pay(String),
    Admin(String, i64, u64),
}

fn decode(name: &str, req: &DynamicMessage) -> Result<Operation, GatewayError> {
    match name {
        "eve_public.dailygoal.api.GetAllCurrentRequest" => Ok(Operation::Current),
        "eve_public.dailygoal.api.GetRequest" => Ok(Operation::Get(goal_id(req)?)),
        "eve_public.dailygoal.api.GetAllWithRewardsRequest" => {
            let page = msg_field(req, "page");
            let size = page.as_ref().and_then(|p| p.get_field_by_name("size")).and_then(|v| v.as_u32()).unwrap_or_default();
            let token = page.as_ref().and_then(|p| p.get_field_by_name("token")).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            Ok(Operation::History(size, token))
        }
        "eve_public.dailygoal.api.RedeemRequest" => {
            let current = req.get_field_by_name("redeem_current_location").and_then(|v| v.as_bool()).ok_or_else(invalid)?;
            Ok(Operation::Redeem(goal_id(req)?, current))
        }
        "eve_public.dailygoal.api.PayForCompletionRequest" => Ok(Operation::Pay(goal_id(req)?)),
        ADMIN_PROGRESS => {
            if !req.has_field_by_name("character") { return Err(invalid()); }
            let character = msg_field(req, "character").ok_or_else(invalid)?;
            let target = character.get_field_by_name("sequential").and_then(|v| v.as_u32()).filter(|id| *id > 0).ok_or_else(invalid)?;
            let increment = req.get_field_by_name("progress").and_then(|v| v.as_u64()).ok_or_else(invalid)?;
            Ok(Operation::Admin(goal_id(req)?, i64::from(target), increment))
        }
        _ => Err(unavailable(name, &[])),
    }
}

pub(super) async fn handle(ctx: &HandlerContext<'_>, world: &Arc<World>, name: &str, request: &DynamicMessage, correlation: &[u8]) -> Result<DynamicMessage, GatewayError> {
    if name == ADMIN_PROGRESS && correlation.len() != UUID_LEN { return Err(invalid()); }
    let user = ctx.require_user()?;
    let identity = ctx.require_online()?;
    let resources = Resources::from_world(world).map_err(error)?;
    let config = ctx.config.game_config().daily_goals;
    if name == ADMIN_PROGRESS {
        daily::admin::authorize(&resources, &identity, user, correlation, &config).await.map_err(error)?;
    }
    let op = decode(name, request)?;
    let mode = match op { Operation::Current | Operation::Get(_) | Operation::History(_, _) => GatewayMode::Read, _ => GatewayMode::Commit };
    let uuid: Option<[u8; UUID_LEN]> = correlation.try_into().ok();
    let invocation = GatewayInvocationIdentity { user_id: Some(user), character_id: Some(identity.char_id), correlation_uuid: uuid };
    let name_owned = SUPPORTED.iter().copied().find(|candidate| *candidate == name).ok_or_else(invalid)?;
    let at = crate::client::filetime_now();
    let operation: GatewayOperation = Box::new(move || Box::pin(async move {
        let result = match op {
            Operation::Current => daily::wire::current_response(&daily::queries::current(&resources, &identity, user, at).await.map_err(error)?),
            Operation::Get(id) => daily::wire::get_response(&daily::queries::get(&resources, &identity, user, &id, at).await.map_err(error)?),
            Operation::History(size, token) => daily::wire::history_response(&daily::queries::history(&resources, &identity, user, size, &token, config.history_page_maximum, at).await.map_err(error)?),
            Operation::Redeem(id, current) => {
                daily::redeem::redeem(&resources, &identity, user, &id, current, at).await.map_err(error)?;
                daily::wire::empty_response(name_owned)
            }
            Operation::Pay(id) => {
                daily::payments::pay_for_completion(&resources, &identity, user, &id, at).await.map_err(error)?;
                daily::wire::empty_response(name_owned)
            }
            Operation::Admin(id, target, increment) => {
                daily::admin::progress(&resources, &identity, user, uuid.as_ref().ok_or_else(invalid)?, target, &id, increment, &config, at).await.map_err(error)?;
                daily::wire::empty_response(name_owned)
            }
        };
        result.map_err(error)
    }));
    gateway::invoke_gateway(name, request, invocation, mode, operation).await
}

#[cfg(test)] mod tests;
