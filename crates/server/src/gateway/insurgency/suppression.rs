//! Suppression thresholds for `messenger.py:GetSuppressionStages`.
//! Uses fixed thresholds and descriptors, not World or DAO data; this does not
//! implement live suppression state.
use anyhow::Result;
use prost_reflect::DynamicMessage;

pub(in crate::gateway) fn handle(name: &str) -> Result<DynamicMessage> {
    super::stage_thresholds(name)
}
