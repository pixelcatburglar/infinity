//! Retained development-index policy history and capability classification.
//! No client module is mirrored; activations come from the shared content
//! catalogue and compiled packages through `RetainedPolicies::resolve`. An
//! empty activation list is a capability, never a fabricated policy.
use std::sync::Arc;
use crate::{dao::lua_content::activations::ActivationRow, lua::{content::{indices::{Definitions, NAMESPACE}, PinnedPolicy}, reload::{HostUnavailable, MissingRetainedPackage}, RetainedPolicies}, services::{db_err, CallError}};
use super::model::{IndexLoadError, IndexUnavailable};

/// One retained policy segment; `starts_at` is the durable activation instant
/// (`max(previous start, published_at)`), so a late-committing publish can never
/// move an earlier segment's start backwards.
#[derive(Debug)]
pub(crate) struct PolicySegment {
    pub ordinal: i64,
    pub starts_at: i64,
    pub definitions: Arc<Definitions>,
}

/// Complete activation identities plus the compiled segments they resolved to.
/// `identities` is the full durable vector so a prepared delivery can recheck
/// it under the namespace fence before writing.
#[derive(Debug)]
pub(crate) struct PolicyHistory {
    /// Immutable activation snapshot shared with the catalogue.
    pub identities: Arc<Vec<ActivationRow>>,
    pub segments: Vec<PolicySegment>,
}

impl PolicyHistory {
    /// Newest retained segment; `load` never produces an empty history.
    pub(crate) fn head(&self) -> &PolicySegment {
        self.segments.last().expect("policy history is never empty")
    }
    /// Segment active at `at`; none before the first activation.
    pub(crate) fn segment_at(&self, at: i64) -> Option<&PolicySegment> {
        self.segments.iter().rev().find(|segment| segment.starts_at <= at)
    }
}

/// Missing retained packages and host outages are typed unavailability;
/// every other failure means present-but-invalid data.
fn classify(error: CallError) -> IndexLoadError {
    match error {
        CallError::Internal(error) => {
            if let Some(missing) = error.downcast_ref::<MissingRetainedPackage>() {
                return IndexLoadError::Unavailable(IndexUnavailable::MissingRetained {
                    namespace: missing.namespace.clone(),
                    version: missing.version.clone(),
                });
            }
            if error.downcast_ref::<HostUnavailable>().is_some() {
                return IndexLoadError::Unavailable(IndexUnavailable::HostUnavailable);
            }
            IndexLoadError::Invalid(CallError::Internal(error))
        }
        error @ CallError::User { .. } => IndexLoadError::Invalid(error),
    }
}

/// Load the complete retained activation history in ordinal order. Every named
/// package is resolved exactly by namespace/version and its stored fingerprint;
/// the current bundle never stands in for an older policy.
pub(crate) fn load(policies: &RetainedPolicies) -> Result<Arc<PolicyHistory>, IndexLoadError> {
    let identities = policies.activations(NAMESPACE)
        .map_err(IndexLoadError::Invalid)?;
    if identities.is_empty() {
        return Err(IndexLoadError::Unavailable(IndexUnavailable::NeverActivated));
    }
    let mut segments = Vec::with_capacity(identities.len());
    let mut starts_at = 0_i64;
    let mut previous_ordinal = 0;
    for row in identities.iter() {
        if row.namespace != NAMESPACE || row.ordinal <= previous_ordinal || row.published_at <= 0 {
            return Err(IndexLoadError::Invalid(db_err(anyhow::anyhow!("invalid development-index activation identity"))));
        }
        previous_ordinal = row.ordinal;
        let fingerprint: [u8; 32] = row.fingerprint.as_slice().try_into()
            .map_err(|_| IndexLoadError::Invalid(db_err(anyhow::anyhow!("invalid development-index activation fingerprint"))))?;
        let policy = policies.resolve(&row.namespace, &row.version).map_err(classify)?;
        if policy.fingerprint() != &fingerprint || policy.namespace() != row.namespace || policy.version() != row.version {
            return Err(IndexLoadError::Invalid(db_err(anyhow::anyhow!("retained development-index package identity mismatch"))));
        }
        let definitions = pinned_definitions(&policy)?;
        starts_at = row.published_at.max(starts_at);
        segments.push(PolicySegment { ordinal: row.ordinal, starts_at, definitions });
    }
    Ok(Arc::new(PolicyHistory { identities, segments }))
}

fn pinned_definitions(policy: &PinnedPolicy) -> Result<Arc<Definitions>, IndexLoadError> {
    policy.indices().cloned()
        .ok_or_else(|| IndexLoadError::Invalid(db_err(anyhow::anyhow!("retained package is not a development-index policy"))))
}

/// Unavailability stays typed inside `CallError::Internal` so callers can
/// downcast it back to `IndexUnavailable`; invalid data surfaces unchanged.
impl From<IndexLoadError> for CallError {
    fn from(error: IndexLoadError) -> Self {
        match error {
            IndexLoadError::Unavailable(unavailable) => CallError::Internal(anyhow::Error::new(unavailable)),
            IndexLoadError::Invalid(error) => error,
        }
    }
}

impl std::fmt::Display for IndexUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeverActivated => f.write_str("development-index policy never activated"),
            Self::HostUnavailable => f.write_str("Lua host unavailable"),
            Self::MissingRetained { namespace, version } => write!(f, "retained development-index package {namespace}/{version} missing"),
        }
    }
}
impl std::error::Error for IndexUnavailable {}


