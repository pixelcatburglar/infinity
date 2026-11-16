//! Ordinary warp request geometry and actor-owned route lifecycle.
//! Destiny movement uses the native ballpark warp command; route intent is transient.
//! Queued routes retain the identity of their original ball.

use super::*;
use super::warp_arrival::WarpArrivalId;

/// Server policy: minimum clearance from an object when warping to it, in metres.
const MIN_OBJECT_WARP_CLEARANCE: f64 = 2500.0;

#[derive(Clone, Copy, Debug)]
pub(crate) enum WarpDestination {
    Ordinary { point: V3, object_radius: Option<f64>, min_range: f64 },
    Exact(V3),
}

impl WarpDestination {
    pub(crate) fn is_valid(self) -> bool {
        match self {
            Self::Ordinary { point, object_radius, min_range } => point.iter().all(|v| v.is_finite())
                && object_radius.is_none_or(f64::is_finite) && min_range.is_finite(),
            Self::Exact(point) => point.iter().all(|v| v.is_finite()),
        }
    }
}

pub(super) struct WarpIntent {
    pub(super) generation: (Option<u64>, Option<u64>),
    pub(super) pending: bool,
    pub(super) ordinary: bool,
    pub(super) full: bool,
    pub(super) arrival: Option<WarpArrivalId>,
}

/// Stand-off for an ordinary warp: object clearance against hull surfaces, or a
/// nonnegative range to a bare point.
pub(crate) fn ordinary_warp_standoff(
    object_radius: Option<f64>, ship_radius: f64, min_range: f64,
) -> f64 {
    match object_radius {
        Some(radius) => radius + ship_radius + min_range.max(MIN_OBJECT_WARP_CLEARANCE),
        None => min_range.max(0.0),
    }
}

fn warp_command(b: &mut Ball, t: i64, factor: i64, destination: WarpDestination) -> (WarpOutcome, Vec<Value>) {
    let (point, stand_off) = match destination {
        WarpDestination::Ordinary { point, object_radius, min_range } => {
            (point, ordinary_warp_standoff(object_radius, b.radius as f64, min_range))
        }
        WarpDestination::Exact(point) => (point, 0.0),
    };
    let outcome = b.warp_to(point, stand_off, factor);
    let events = match outcome {
        WarpOutcome::Ignored => vec![],
        WarpOutcome::Approach => vec![event(t, "GotoPoint", vec![Value::Int(b.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])],
        WarpOutcome::Align => {
            b.set_speed_fraction(1.0);
            vec![
                event(t, "WarpTo", vec![Value::Int(b.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2]), Value::Float(stand_off), Value::Int(factor)]),
                event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]),
                special_fx(t, b.id, None, None, None, "effects.Warping", false, 1, 0,
                    Value::Int(SPECIAL_FX_UNSPECIFIED_DURATION), 0, None, 0, Value::None),
            ]
        }
    };
    (outcome, events)
}

impl System {
    pub(super) fn cancel_warp_intent(&mut self, ship: i64) {
        if let Some(intent) = self.warp_exits.remove(&ship) {
            if let Some(id) = intent.arrival { self.release_warp_arrival(id); }
        }
    }

    pub(super) fn detach_warp_arrival(&mut self, ship: i64) {
        let Some(intent) = self.warp_exits.get_mut(&ship) else { return };
        let id = intent.arrival.take();
        if !intent.ordinary { self.warp_exits.remove(&ship); }
        if let Some(id) = id { self.release_warp_arrival(id); }
    }

    /// Queue one warp request into the scene action prefix. Position and
    /// bubble changes ride the same ordered stream; real warp exits still
    /// invalidate only their own original rules.
    pub(crate) fn queue_warp(&mut self, ship: i64, factor: i64, destination: WarpDestination, arrival: Option<WarpArrivalId>) -> bool {
        if !destination.is_valid() || !self.balls.contains_key(&ship) { return false; }
        let arrival = arrival.filter(|id| self.retain_warp_arrival(*id));
        let token = self.capture_ball(ship).expect("existing warp ship");
        let generation = (self.persistent_generation(ship), self.combat_membership_generation(ship));
        self.actions.push_back((self.current_time, Box::new(move |s, stamp| {
            if !s.ball_token_current(token) || (s.persistent_generation(ship), s.combat_membership_generation(ship)) != generation {
                if let Some(id) = arrival { s.release_warp_arrival(id); }
                return;
            }
            let mut outcome = WarpOutcome::Ignored;
            let collisions = s.scene_action_collisions();
            s.apply_command(ship, stamp, collisions, |ball, stamp, _| {
                let (actual, events) = warp_command(ball, stamp, factor, destination);
                outcome = actual;
                events
            });
            match outcome {
                WarpOutcome::Align => {
                    s.cancel_warp_intent(ship);
                    s.warp_exits.insert(ship, WarpIntent { generation, ordinary: true, pending: true, full: true, arrival });
                }
                WarpOutcome::Approach => {
                    s.cancel_warp_intent(ship);
                    if let Some(id) = arrival { s.complete_warp_arrival(id); }
                }
                WarpOutcome::Ignored => {
                    s.detach_warp_arrival(ship);
                    if let Some(intent) = s.warp_exits.get_mut(&ship) {
                        if intent.full { intent.arrival = arrival; }
                        else if let Some(id) = arrival { s.release_warp_arrival(id); }
                    } else if let Some(id) = arrival {
                        s.warp_exits.insert(ship, WarpIntent { generation, ordinary: false, pending: false, full: true, arrival: Some(id) });
                    }
                }
            }
        })));
        true
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
