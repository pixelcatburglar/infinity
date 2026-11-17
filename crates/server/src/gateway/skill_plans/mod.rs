//! Personal and corporation plan routing for the public gateway client.
//! Adapters parse descriptors and encode service results; business rules use shared services.
pub(super) mod corporation;
pub(super) mod corporation_milestones;
pub(super) mod personal;
pub(super) mod personal_milestones;
mod notices;
mod wire;

fn error(error: crate::services::skill_plans::Error) -> super::error::GatewayError {
    use crate::services::skill_plans::Error;
    use super::error::{FailureCode, GatewayError};
    match error {
        Error::Invalid => GatewayError::refusal(FailureCode::BadRequest),
        Error::Missing => GatewayError::refusal(FailureCode::NotFound),
        Error::Precondition => GatewayError::refusal(FailureCode::PreconditionFailed),
        Error::Internal(crate::services::CallError::Internal(error)) => GatewayError::Internal(error),
        Error::Internal(crate::services::CallError::User { .. }) => GatewayError::refusal(FailureCode::PreconditionFailed),
    }
}
#[cfg(all(test, feature = "tests-gateway"))]
mod tests;
