//! Server-selected civilian rookie fits, keyed by SDE race ship type.
//! Character selection grants these persistent items; there is no client fit mirror,
//! no charges and no retroactive refit of existing ships.

use crate::consts::{FALLBACK_ROOKIE_SHIP, FLAG_DRONE_BAY, FLAG_FITTING_FIRST, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST};

/// SDE invTypes: Ibis.
const IBIS: i32 = 601;
/// SDE invTypes: Reaper.
const REAPER: i32 = 588;
/// SDE invTypes: Impairor.
const IMPAIROR: i32 = 596;
/// SDE invTypes: civilian Gatling railgun.
const RAILGUN: i32 = 3638;
/// SDE invTypes: civilian autocannon.
const AUTOCANNON: i32 = 3636;
/// SDE invTypes: civilian pulse laser.
const LASER: i32 = 3634;
/// SDE invTypes: civilian electron blaster.
const BLASTER: i32 = 3640;
/// SDE invTypes: civilian miner.
const MINER: i32 = 3651;
/// SDE invTypes: civilian shield booster.
const SHIELD_BOOSTER: i32 = 10039;
/// SDE invTypes: civilian afterburner.
const AFTERBURNER: i32 = 21857;
/// SDE invTypes: civilian armor repairer.
const ARMOR_REPAIRER: i32 = 21853;
/// SDE invTypes: civilian damage control.
const DAMAGE_CONTROL: i32 = 30839;
/// SDE invTypes: civilian drone.
const DRONE: i32 = 32465;

pub(super) struct RookieFit {
    pub ship: i32,
    pub items: &'static [(i32, i32, i32)],
}

/// Server design: these fits are not licensed live-server loadouts.
pub(super) fn for_ship(ship: i32) -> Option<RookieFit> {
    let items: &'static [(i32, i32, i32)] = match ship {
        IBIS => &[(RAILGUN, FLAG_HI_SLOT_FIRST, 1), (MINER, FLAG_HI_SLOT_FIRST + 1, 1), (SHIELD_BOOSTER, FLAG_MED_SLOT_FIRST, 1), (AFTERBURNER, FLAG_MED_SLOT_FIRST + 1, 1), (DAMAGE_CONTROL, FLAG_FITTING_FIRST, 1), (DRONE, FLAG_DRONE_BAY, 1)],
        REAPER => &[(AUTOCANNON, FLAG_HI_SLOT_FIRST, 1), (MINER, FLAG_HI_SLOT_FIRST + 1, 1), (SHIELD_BOOSTER, FLAG_MED_SLOT_FIRST, 1), (AFTERBURNER, FLAG_MED_SLOT_FIRST + 1, 1), (DAMAGE_CONTROL, FLAG_FITTING_FIRST, 1), (DRONE, FLAG_DRONE_BAY, 1)],
        IMPAIROR => &[(LASER, FLAG_HI_SLOT_FIRST, 1), (MINER, FLAG_HI_SLOT_FIRST + 1, 1), (AFTERBURNER, FLAG_MED_SLOT_FIRST, 1), (ARMOR_REPAIRER, FLAG_FITTING_FIRST, 1), (DAMAGE_CONTROL, FLAG_FITTING_FIRST + 1, 1), (DRONE, FLAG_DRONE_BAY, 1)],
        FALLBACK_ROOKIE_SHIP => &[(BLASTER, FLAG_HI_SLOT_FIRST, 1), (MINER, FLAG_HI_SLOT_FIRST + 1, 1), (AFTERBURNER, FLAG_MED_SLOT_FIRST, 1), (ARMOR_REPAIRER, FLAG_FITTING_FIRST, 1), (DAMAGE_CONTROL, FLAG_FITTING_FIRST + 1, 1), (DRONE, FLAG_DRONE_BAY, 1)],
        _ => return None,
    };
    Some(RookieFit { ship, items })
}
