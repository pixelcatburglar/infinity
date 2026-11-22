//! Command-burst special effects: the source sphere and the per-recipient
//! link, mirroring `dbuff/common/baseBurstEffect.py` (`sourceVFXEffect` and
//! `targetVFXEffect` per family). The SDE effect rows 6732-6736 carry an
//! empty guid, so the client effect names live here; encoding uses the
//! shared `special_fx`. No sustained or replayed FX record and no other
//! animation is owned here.

use super::command_burst::CommandBurstFamily;
use crate::space::{special_fx, FxModule, System};
use marshal::Value;

/// `(sourceVFXEffect, targetVFXEffect)` client effect names of one family.
pub(crate) fn command_burst_guids(family: CommandBurstFamily) -> (&'static str, &'static str) {
    match family {
        CommandBurstFamily::Armor => ("effects.WarfareLinkSphereArmor", "effects.WarfareLinkArmor"),
        CommandBurstFamily::Information => ("effects.WarfareLinkSphereInformation", "effects.WarfareLinkInformation"),
        CommandBurstFamily::Mining => ("effects.WarfareLinkSphereMining", "effects.WarfareLinkMining"),
        CommandBurstFamily::Shield => ("effects.WarfareLinkSphereShield", "effects.WarfareLinkShield"),
        CommandBurstFamily::Skirmish => ("effects.WarfareLinkSphereSkirmish", "effects.WarfareLinkSkirmish"),
    }
}

/// Publish one transient burst effect about `ship`: the sphere when `target`
/// is None (the source), the link when `target` names the source ship (a
/// recipient). Fourteen OnSpecialFX arguments, start=1, active=0.
pub(crate) fn emit_command_burst_fx(system: &mut System, ship: i64, module: Option<FxModule>, target: Option<i64>, family: CommandBurstFamily, duration: Value) {
    let (sphere, link) = command_burst_guids(family);
    let guid = if target.is_some() { link } else { sphere };
    system.publish_destiny(ship, move |stamp| vec![special_fx(stamp, ship, module, target, None, guid, false, 1, 0, duration, 0, None, 0, Value::None)]);
}
