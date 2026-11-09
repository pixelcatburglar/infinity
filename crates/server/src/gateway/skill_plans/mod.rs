//! Personal plan routing for the public gateway client.
//! Adapters parse descriptors and encode service results; business rules use shared services.
//! Corporation plan requests are not claimed here.
pub(super) mod personal;
pub(super) mod personal_milestones;

fn error(error: crate::services::skill_plans::Error) -> super::error::GatewayError {
    use crate::services::skill_plans::Error;
    use super::error::{FailureCode, GatewayError};
    match error {
        Error::Invalid => GatewayError::refusal(FailureCode::BadRequest, infinity_core::messages::skill_plans::INVALID),
        Error::Missing => GatewayError::refusal(FailureCode::NotFound, infinity_core::messages::skill_plans::INVALID),
        Error::Precondition => GatewayError::refusal(FailureCode::PreconditionFailed, infinity_core::messages::skill_plans::INVALID),
        Error::Internal(crate::services::CallError::Internal(error)) => GatewayError::Internal(error),
        Error::Internal(crate::services::CallError::User { .. }) => GatewayError::refusal(FailureCode::PreconditionFailed, infinity_core::messages::skill_plans::INVALID),
    }
}
