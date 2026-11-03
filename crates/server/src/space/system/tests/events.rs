//! Test-only access to the system's collision-policy tick.

use crate::space::System;

pub(crate) fn tick_with_collisions(system: &mut System, ms: i64, collisions: bool) {
    system.tick_with_collisions(ms, collisions);
}
