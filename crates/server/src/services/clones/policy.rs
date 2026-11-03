//! Jump-clone cooldown calculation for jumpCloneSvc.
//!
//! Uses operator-configured hours and skill reduction; implants and ship-borne
//! clone vat bays are not modelled.

use crate::consts::FILETIME_HOUR;

pub(super) fn jump_cooldown(base_hours: i64, reduction_per_level: i64, level: i64) -> i128 {
    let hours = (i128::from(base_hours) - i128::from(level.max(0)) * i128::from(reduction_per_level)).max(0);
    hours * i128::from(FILETIME_HOUR)
}

#[cfg(test)]
mod tests;
