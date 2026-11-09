//! Committed sovereignty state published to the space actor after transaction commit.
//! Rows come from the sovereignty DAO; no client protocol is encoded here.
//! Capture links and their retained observation segments are owned by space.
use std::sync::Arc;
use crate::{db::Db, sde::Sde, space::World};
use crate::dao::sovereignty::{capture::CaptureRow, policy::PolicyRow, sources::SourceChange, structures::HubRow, systems::SystemRow};

#[derive(Debug)]
pub(crate) struct StalePlan;
impl std::fmt::Display for StalePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("sovereignty dependencies changed") }
}
impl std::error::Error for StalePlan {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionKind {
    Deployed, Claimed, Transferred, Neutralized, DestructRequested,
    DestructCancelled, DestructFinished, RecoveryCancelled,
}

impl TransitionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Deployed => "deployed",
            Self::Claimed => "claimed",
            Self::Transferred => "transferred",
            Self::Neutralized => "neutralized",
            Self::DestructRequested => "destruct_requested",
            Self::DestructCancelled => "destruct_cancelled",
            Self::DestructFinished => "destruct_finished",
            Self::RecoveryCancelled => "recovery_cancelled",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Resources {
    pub db: Db,
    pub sde: Arc<Sde>,
    pub world: Arc<World>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlacementObstacle {
    pub group_id: i32,
    pub position: crate::space::V3,
}

#[derive(Clone, Debug)]
pub(crate) struct PlacementProof {
    pub ship_id: i64,
    pub character_id: i64,
    pub position: crate::space::V3,
    pub obstacles: Vec<PlacementObstacle>,
}

#[derive(Clone, Debug)]
pub(crate) struct HubPublication {
    pub system: SystemRow,
    pub hub: Option<HubRow>,
    pub capture: Option<CaptureRow>,
}

/// Current alliance policy and whether its capital system changed in this commit.
#[derive(Clone, Debug)]
pub(crate) struct CapitalPublication {
    pub policy: PolicyRow,
    pub changed: bool,
}

impl CapitalPublication {
    pub(crate) fn new(policy: PolicyRow, previous: Option<i64>) -> Self {
        let changed = policy.current_capital_system != previous;
        Self { policy, changed }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CommittedPublication {
    pub sources: Vec<SourceChange>,
    pub hubs: Vec<HubPublication>,
    pub capitals: Vec<CapitalPublication>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureContributor {
    pub character_id: i64,
    pub ship_id: i64,
    pub module_id: i64,
    pub corporation_id: i64,
    pub alliance_id: i64,
    pub source_revision: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct CaptureContribution {
    pub contributor: CaptureContributor,
    pub through: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct CaptureObservation {
    pub target: CaptureRow,
    pub system_revision: i64,
    pub contributors: Vec<CaptureContribution>,
    pub claimant: Option<CaptureContributor>,
}
