//! Typed gateway failures for requestBroker.py response handling.
//! Uses DAO errors and server diagnostics; it does not expose diagnostics to clients.
//! Authentication and gameplay validation are handled by request consumers.

#[derive(Debug)]
pub(super) enum FailureCode { BadRequest, Unauthorized, NotImplemented, Internal }
impl FailureCode {
    pub(super) fn status(&self) -> u32 {
        let status = match self {
            Self::BadRequest => http::StatusCode::BAD_REQUEST,
            Self::Unauthorized => http::StatusCode::UNAUTHORIZED,
            Self::NotImplemented => http::StatusCode::NOT_IMPLEMENTED,
            Self::Internal => http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        u32::from(status.as_u16())
    }
}

#[derive(Debug)]
pub(super) enum GatewayError {
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
