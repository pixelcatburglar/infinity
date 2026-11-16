//! Shared static public-system selection for space tests.

use std::sync::Arc;

use infinity_core::consts::GROUP_ASTEROID_BELT;

/// The nth public system that has a celestial and no asteroid belt.
pub(in crate::space) fn public_static_system(sde: &Arc<crate::sde::Sde>, nth: usize) -> i64 {
    sde.map.public_systems().map(|s| s.solar_system_id).filter(|&id|
        sde.map.celestials(id).next().is_some() && !sde.map.celestials(id).any(|c| c.group_id == Some(GROUP_ASTEROID_BELT as i64)))
        .nth(nth).unwrap()
}
