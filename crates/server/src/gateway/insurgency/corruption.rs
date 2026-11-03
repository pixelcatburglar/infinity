//! Corruption thresholds for `messenger.py:GetCorruptionStages`.
//! Uses fixed thresholds and descriptors, not World or DAO data; this does not
//! implement live corruption state.
use anyhow::Result;
use prost_reflect::DynamicMessage;

pub(in crate::gateway) fn handle(name: &str) -> Result<DynamicMessage> {
    super::stage_thresholds(name)
}
