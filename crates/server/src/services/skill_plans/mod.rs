//! Personal skill plans and viewer-specific milestones.
//! Mirrors the public skill-plan gateway and client validation using game skill-plan tables.
//! Corporation plans and completion notifications are not provided here.
pub(crate) mod personal;
pub(crate) mod milestones;

use infinity_core::consts::UUID_LEN;
fn valid_id(id: &[u8]) -> bool { id.len() == UUID_LEN && id.iter().any(|byte| *byte != 0) }

#[derive(Debug)]
pub(crate) enum Error {
    Invalid,
    Missing,
    Precondition,
    Internal(crate::services::CallError),
}

impl From<crate::db::Error> for Error {
    fn from(error: crate::db::Error) -> Self { Self::Internal(crate::services::db_err(error)) }
}
impl From<crate::services::CallError> for Error {
    fn from(error: crate::services::CallError) -> Self {
        match error {
            crate::services::CallError::User { .. } => Self::Precondition,
            error => Self::Internal(error),
        }
    }
}
impl crate::db::transaction::TransactionError for Error {
    fn is_retryable_transaction(&self) -> bool {
        match self {
            Self::Internal(error) => crate::db::transaction::TransactionError::is_retryable_transaction(error),
            _ => false,
        }
    }
}
pub(crate) mod progress;
pub(crate) mod recovery;
pub(crate) mod validation;
#[cfg(test)] mod tests;
