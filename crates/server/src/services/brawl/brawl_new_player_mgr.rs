//! Closed novice brawl operations consumed by `brawl/client/brawl_novice_guide_controller.py`.
//! No data source is used; novice fights and their gameplay are not implemented.

use super::super::{notify_error, Call, CallResult};
use infinity_core::messages;

pub(super) fn dispatch(call: &Call<'_>) -> Option<CallResult> {
    match (call.service, call.method) {
        ("brawlNewPlayerMgr", "EnableSkillUpgrade" | "Step3" | "ChangeZoneStatusToPreset" | "ActiveGate" | "GenerateCloud" | "CollectCloud" | "ReleaseCloud" | "SpawnFakePlayer" | "UnlimitedEntropy" | "ActiveEntropyModule" | "Leave" | "Step2" | "Step1") =>
            Some(Err(notify_error(messages::brawl::UNAVAILABLE))),
        _ => None,
    }
}
