//! Typed gateway failures for requestBroker.py response handling.
//! Uses DAO errors and server diagnostics; it does not expose diagnostics to clients.
//! Authentication and gameplay validation are handled by request consumers.

#[derive(Debug, Clone, Copy)]
pub(crate) enum FailureCode { BadRequest, Unauthorized, Forbidden, NotFound, PreconditionFailed, NotImplemented, Internal }
impl FailureCode {
    pub(super) fn status(&self) -> u32 {
        let status = match self {
            Self::BadRequest => http::StatusCode::BAD_REQUEST,
            Self::Unauthorized => http::StatusCode::UNAUTHORIZED,
            Self::Forbidden => http::StatusCode::FORBIDDEN,
            Self::NotFound => http::StatusCode::NOT_FOUND,
            Self::PreconditionFailed => http::StatusCode::PRECONDITION_FAILED,
            Self::NotImplemented => http::StatusCode::NOT_IMPLEMENTED,
            Self::Internal => http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        u32::from(status.as_u16())
    }
}
#[derive(Debug)]
pub(crate) enum GatewayError {
    Refused { code: FailureCode, message: &'static str },
    Database(crate::db::Error),
    Internal(anyhow::Error),
}

impl From<crate::db::Error> for GatewayError {
    fn from(error: crate::db::Error) -> Self { Self::Database(error) }
}

impl GatewayError {
    pub(super) fn refusal(code: FailureCode, message: &'static str) -> Self {
        Self::Refused { code, message }
    }
}
