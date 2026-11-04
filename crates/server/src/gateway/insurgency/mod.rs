//! Threshold encoding for `messenger.py:_BlockingRequestStageThreasholds`.
//! Uses fixed stage thresholds and descriptors, not World or DAO data; it does
//! not implement live insurgency state.
pub(super) mod corruption;
pub(super) mod suppression;
use anyhow::Result;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use super::message::{response_for, child, set};

/// Server representation of the five equal insurgency stages.
const INSURGENCY_STAGE_COUNT: u64 = 5;
/// Insurgency stages: five equal steps of corruption / suppression.
fn stage_thresholds(request: &str) -> Result<DynamicMessage> {
    let mut rsp = response_for(request)?;
    let mut thresholds = vec![];
    for k in 1..=INSURGENCY_STAGE_COUNT {
        let mut f = child(&rsp.descriptor(), "thresholds")?;
        set(&mut f, "numerator", PV::U64(k))?;
        set(&mut f, "denominator", PV::U64(INSURGENCY_STAGE_COUNT))?;
        thresholds.push(PV::Message(f));
    }
    set(&mut rsp, "thresholds", PV::List(thresholds))?;
    Ok(rsp)
}

#[cfg(test)] mod tests;
