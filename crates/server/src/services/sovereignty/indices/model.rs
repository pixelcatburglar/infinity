//! Shared typed values of the development-index domain.
//! Occurrence order is the deterministic `(occurred_at, source_namespace,
//! source_key, ordinal, slice_id)` tuple; event UUIDs and journal positions are
//! never business order. No client protocol is encoded here.

use std::sync::Arc;
use crate::{dao::{progress::sovereignty::{FactOrder, Increment}, sovereignty::development::DevelopmentRow}, services::CallError};
use super::activation::PolicyHistory;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceSlice {
    pub order: FactOrder,
    pub increment: Increment,
}

/// One system's fact set extracted from one event envelope.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SystemFact {
    pub system_id: i64,
    pub slices: Vec<SourceSlice>,
}

/// Wall-clock anchored military and industrial points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointState {
    pub at: i64,
    pub military: f64,
    pub industrial: f64,
}

/// Client `GetIndexLevel` shape: the band index and the fraction through it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct IndexLevel {
    pub level: i64,
    pub remainder: f64,
}

/// One system's projected state at one instant; strategic age is in seconds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DevelopmentView {
    pub system_id: i64,
    pub military_points: f64,
    pub industrial_points: f64,
    pub strategic_seconds: f64,
    pub military_increasing: bool,
    pub industrial_increasing: bool,
    pub strategic_increasing: bool,
    pub military: IndexLevel,
    pub industrial: IndexLevel,
    pub strategic: IndexLevel,
    pub defense_multiplier: f64,
}

/// Committed development state published to the space actor.
#[derive(Clone, Debug)]
pub(crate) struct DevelopmentPublication {
    pub system_id: i64,
    pub row: Option<DevelopmentRow>,
    pub policies: Arc<PolicyHistory>,
}

/// Why the whole capability cannot serve; distinct from available-but-invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IndexUnavailable {
    NeverActivated,
    HostUnavailable,
    MissingRetained { namespace: String, version: String },
}

#[derive(Debug)]
pub(crate) enum IndexLoadError {
    Unavailable(IndexUnavailable),
    Invalid(CallError),
}

/// Last resolved capability snapshot; the slot is a cache, never the authority.
#[derive(Clone)]
pub(crate) enum IndexCapability {
    Unavailable(IndexUnavailable),
    Ready(Arc<PolicyHistory>),
    Invalid(Arc<str>),
}

impl std::fmt::Debug for IndexCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(reason) => f.debug_tuple("Unavailable").field(reason).finish(),
            Self::Ready(_) => f.write_str("Ready"),
            Self::Invalid(diagnostic) => f.debug_tuple("Invalid").field(diagnostic).finish(),
        }
    }
}
