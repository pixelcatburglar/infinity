//! Empty directory compatibility for unsupported player structures.
//! Mirrors `eve/client/script/ui/services/structure/structureDirectory.py` and
//! the map and market readers. No database or runtime state is accessed;
//! structure gameplay and directory methods not listed here are unsupported.

use marshal::Value;
use super::{Call, CallResult, notify_error};

pub(super) fn dispatch(call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("structureDirectory", "GetMyCharacterStructures" | "GetMyCorporationStructures") => Ok(Value::Dict(vec![])),
        ("structureDirectory", "GetStructureMapData" | "GetMyDockableStructures" | "GetSolarSystemsWithBeacons" | "GetNearbyJumpBridges") => Ok(Value::list([])),
        ("structureDirectory", "GetJumpBridgesWithMyAccess") => Ok(Value::tuple([Value::list([]), Value::list([]), Value::list([])])),
        ("structureDirectory", "GetStructureDescription") => Ok(Value::unicode("")),
        ("structureDirectory", "SetStructureDescription") => Err(notify_error(infinity_core::messages::structures::UNAVAILABLE)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
