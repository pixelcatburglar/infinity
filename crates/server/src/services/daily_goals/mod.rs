//! Daily-goal assignment, retained views and public protocol projection.
//! Definitions come from retained authored Lua content and game daily-goal tables.
//! Progress and reward settlement are handled by separate operations.

pub(crate) mod activation;
pub(crate) mod admin;
pub(crate) mod assignment;
pub(crate) mod facts;
pub(crate) mod frozen;
pub(crate) mod progress;
pub(crate) mod payments;
pub(crate) mod qualification;
pub(crate) mod queries;
pub(crate) mod redeem;
pub(crate) mod rollover;
pub(crate) mod wire;
pub(crate) mod ids;

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use crate::{dao::daily_goals::{instances::GoalRow, rewards::RewardRow}, db::Db, lua::{self, content::daily_goals::model::Template}, sde::Sde, services::CallError, space::World};

/// Server-owned namespace of the daily authored content package.
pub(crate) const DAILY_GOALS_NAMESPACE: &str = "daily_goals";

#[derive(Clone)]
pub(crate) struct Resources {
    pub db: Db,
    pub world: Arc<World>,
    pub sde: Arc<Sde>,
    pub policies: lua::RetainedPolicies,
}
impl Resources {
    pub(crate) fn from_world(world: &Arc<World>) -> Result<Self, Error> {
        if !lua::enabled() { return Err(Error::Unavailable); }
        Ok(Self { db: world.db().clone(), world: Arc::clone(world), sde: Arc::clone(world.sde()), policies: lua::retained_policies()? })
    }
}

#[derive(Debug)]
pub(crate) enum Error { Invalid, Unauthenticated, Missing, Forbidden, Unavailable, Internal(CallError) }
impl From<crate::db::Error> for Error {
    fn from(error: crate::db::Error) -> Self { Self::Internal(error.into()) }
}
impl From<CallError> for Error {
    fn from(error: CallError) -> Self {
        match error {
            CallError::User { .. } => Self::Forbidden,
            other => Self::Internal(other),
        }
    }
}
impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self { Self::Internal(error.into()) }
}
impl From<mlua::Error> for Error {
    fn from(error: mlua::Error) -> Self { Self::Internal(crate::services::db_err(error)) }
}
impl crate::db::transaction::TransactionError for Error {
    fn is_retryable_transaction(&self) -> bool {
        match self {
            Self::Internal(error) => crate::db::transaction::TransactionError::is_retryable_transaction(error),
            _ => false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageRefs { pub name: u64, pub description: u64, pub help: u64 }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GoalSnapshot { pub template: Template, pub messages: MessageRefs, pub assigner: i64 }
pub(crate) struct GoalView { pub goal: GoalRow, pub snapshot: GoalSnapshot, pub rewards: Vec<RewardRow>, pub current_omega: bool }
pub(crate) struct HistoryPage { pub ids: Vec<String>, pub next_token: String }
