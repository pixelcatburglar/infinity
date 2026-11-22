//! Ship-sourced warp-disruption fields held by the system actor.
//! Destiny itself carries the native warp (`eve/client/script/remote/michelle.py`
//! consumes Stop and WarpTo); the field is actor state, not a projected ball or
//! a scene object. Its centre follows the source ship's sampled position, so a
//! moving heavy interdictor drags its field along. Fields are transient: a
//! module stop or the source leaving the scene retires them, and nothing is
//! persisted or read from the database.
//! Raising a field scans the ballpark once and cancels ships still aligning
//! inside; a ship already in warp is never grabbed mid-flight. Warp admission
//! refuses ships intersecting a field, and activation-time landing drags a
//! route whose entry point lies near its original destination to the field
//! edge. A launched warp probe raises the same field on its own stationary
//! ball; mobile warp disruptors are not supported.

use super::*;
use crate::services::fit::Key;
use infinity_core::consts::dogma::ATTR_WARP_BUBBLE_IMMUNE;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Server policy: a route whose field entry point lies within 500 km of its
/// original destination is dragged to that entry point, in metres.
const CATCH_DISTANCE_M: f64 = 500_000.0;

/// One live field: the source ship ball supplies its sampled centre, `radius`
/// its reach in metres, and `generation` the owning activation's stop flag so
/// a stale retirement cannot clear a replacement field raised by the same
/// module.
pub(super) struct Field {
    source: i64,
    radius: f64,
    generation: Arc<AtomicBool>,
}

/// Live fields keyed by the module that raised them.
#[derive(Default)]
pub(super) struct Fields {
    by_module: HashMap<i64, Field>,
}

impl System {
    /// Register a field owned by `module` on `ship`. Refused without a live
    /// source ball, a usable finite radius or an already-retired generation;
    /// a refusal leaves no partial state. Once registered, the ballpark is
    /// scanned exactly once: every ship still aligning for warp inside the
    /// field (including the source itself) is queued the shared Stop. Ships
    /// already in warp are not touched.
    pub(crate) fn begin_interdiction(&mut self, ship: i64, module: i64, generation: Arc<AtomicBool>, radius: f64) -> bool {
        if generation.load(Ordering::Relaxed) || !radius.is_finite() || radius <= 0.0 { return false; }
        let Some(center) = self.ball(ship).map(|source| source.position_at(self.ms)) else { return false; };
        self.interdiction.by_module.insert(module, Field { source: ship, radius, generation });
        let stops: Vec<(i64, u64)> = self.balls.values()
            .filter(|ball| ball.mode == MODE_WARP && ball.effect_stamp < 0)
            .filter(|ball| dist(center, ball.position_at(self.ms)) <= radius + f64::from(ball.radius))
            .filter(|ball| !self.interdiction_immune(ball.id))
            .filter_map(|ball| self.combat_membership_generation(ball.id).map(|membership| (ball.id, membership)))
            .collect();
        for (id, membership) in stops { self.stop_aligning_warp(id, membership); }
        true
    }

    /// Retire the field of `module` only when the caller carries its exact
    /// activation generation; an older callback cannot remove a replacement.
    /// A set stop flag only requests retirement at cycle end: the field stays
    /// live until this call or the source's departure removes it.
    pub(crate) fn end_interdiction(&mut self, module: i64, generation: &Arc<AtomicBool>) {
        if self.interdiction.by_module.get(&module).is_some_and(|field| Arc::ptr_eq(&field.generation, generation)) {
            self.interdiction.by_module.remove(&module);
        }
    }

    /// The ship's sampled position intersects a live field, hull surface
    /// included. Warp scramble status is not consulted; a positive fitted or
    /// innate `warpBubbleImmune` makes the ship pass.
    pub(crate) fn interdicted(&self, ship: i64) -> bool {
        let Some(ball) = self.ball(ship) else { return false; };
        if self.interdiction_immune(ship) { return false; }
        let pos = ball.position_at(self.ms);
        self.interdiction.by_module.values().any(|field| {
            self.ball(field.source).is_some_and(|source|
                dist(source.position_at(self.ms), pos) <= field.radius + f64::from(ball.radius))
        })
    }

    /// Entry point of the nearest field the route from `origin` to `endpoint`
    /// enters, or `None` when no field catches it. The route is a ray: a field
    /// whose entry point lies within `CATCH_DISTANCE_M` beyond the destination
    /// still catches, while fields behind the origin, off to the side or
    /// farther away do not. The smallest entry distance wins; equal distances
    /// resolve to the lower module ID.
    pub(super) fn interdiction_landing(&self, ship: i64, origin: V3, endpoint: V3) -> Option<V3> {
        if self.interdiction_immune(ship) { return None; }
        let target = dist(origin, endpoint);
        let direction = norm(sub(endpoint, origin));
        if !target.is_finite() || target == 0.0 { return None; }
        let mut best: Option<(f64, i64)> = None;
        for (&module, field) in &self.interdiction.by_module {
            let Some(source) = self.ball(field.source) else { continue };
            let offset = sub(source.position_at(self.ms), origin);
            let projection = dot(offset, direction);
            let rejection = sub(offset, scale(direction, projection));
            let perpendicular_sq = dot(rejection, rejection);
            if perpendicular_sq > field.radius * field.radius { continue; }
            // A field behind the route never catches; an origin inside it
            // enters at the origin itself.
            if projection < 0.0 { continue; }
            let entry = (projection - (field.radius * field.radius - perpendicular_sq).sqrt()).max(0.0);
            if (entry - target).abs() > CATCH_DISTANCE_M { continue; }
            if best.is_none_or(|(best_entry, best_module)| entry < best_entry || (entry == best_entry && module < best_module)) {
                best = Some((entry, module));
            }
        }
        best.map(|(entry, _)| add(origin, scale(direction, entry)))
    }

    /// Drop every field raised by the departing ball.
    pub(super) fn interdiction_departed(&mut self, ball: i64) {
        self.interdiction.by_module.retain(|_, field| field.source != ball);
    }

    /// Positive effective `warpBubbleImmune` on the resident fit decides;
    /// without a combat record the hull's static type attribute answers.
    fn interdiction_immune(&self, ship: i64) -> bool {
        if let Some(record) = self.actor_combat.get(&ship) {
            return record.fit.value(Key::Ship, ATTR_WARP_BUBBLE_IMMUNE) > 0.0;
        }
        self.ball(ship).zip(self.sde()).is_some_and(|(ball, sde)|
            sde.types.attribute(ball.type_id, ATTR_WARP_BUBBLE_IMMUNE).unwrap_or(0.0) > 0.0)
    }
}

#[cfg(all(test, feature = "tests-space"))]
#[path = "interdiction/tests.rs"]
mod tests;
