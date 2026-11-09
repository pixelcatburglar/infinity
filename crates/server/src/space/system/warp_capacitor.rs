//! Activation-time capacitor payment and completed-state warp correction.
//! Fitted dogma supplies pricing; capacitor state remains process-local.

use super::*;
use infinity_core::consts::dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CHARGE, ATTR_RECHARGE_RATE, ATTR_WARP_CAPACITOR_NEED};
use crate::services::combat::groups_health::drain_warp_capacitor;
use crate::services::effects::notify::attribute_change;
use crate::services::fit::Key;

impl System {
    pub(super) fn prepare_warp_activation(&mut self, ship: i64, stamp: i64, logical_ms: i64) {
        let Some(intent) = self.warp_exits.get(&ship) else { return };
        if !intent.pending { return; }
        let Some(ball) = self.balls.get(&ship) else { self.cancel_warp_intent(ship); return };
        if !ball.free || logical_ms <= ball.last_ms || ball.mass * f64::from(ball.agility) <= 0.0 || !ball.is_aligned_for_warp() { return; }
        let generation = (self.persistent_generation(ship), self.combat_membership_generation(ship));
        let endpoint = ball.resolved_warp_endpoint();
        let pos = ball.pos;
        let mass = ball.mass;
        let pilot = ball.char_id;
        let record = self.actor_combat.get(&ship);
        let valid_record = generation == intent.generation && record.is_some_and(|r| r.phase == combat::CombatPhase::Live);
        let values = record.filter(|_| valid_record).map(|r| {
            let coefficient = r.fit.value(Key::Ship, ATTR_WARP_CAPACITOR_NEED);
            let cost = coefficient * mass * (dist(pos, endpoint) / AU);
            let capacity_tau = if cost > 0.0 {
                Some((r.fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), r.fit.value(Key::Ship, ATTR_RECHARGE_RATE)))
            } else { None };
            (coefficient, cost, capacity_tau)
        });
        let valid = values.is_some_and(|(coefficient, cost, capacity_tau)| {
            coefficient.is_finite() && coefficient >= 0.0 && mass.is_finite() && mass >= 0.0
                && pos.iter().chain(endpoint.iter()).all(|v| v.is_finite()) && cost.is_finite() && cost >= 0.0
                && capacity_tau.is_none_or(|(capacity, tau)| capacity.is_finite() && capacity >= 0.0 && tau.is_finite() && tau >= 0.0)
        });
        if !valid {
            tracing::error!(ship, "invalid warp capacitor state");
            self.stop_unpaid_warp(ship, stamp);
            return;
        }
        let (_, cost, capacity_tau) = values.unwrap();
        if cost == 0.0 {
            self.balls.get_mut(&ship).unwrap().activate_warp_to(stamp, endpoint);
            self.warp_exits.get_mut(&ship).unwrap().pending = false;
            return;
        }
        let (capacity, tau) = capacity_tau.unwrap();
        let debit = drain_warp_capacitor(ship, capacity, tau, cost, crate::client::filetime_now());
        if debit.drained > 0.0 {
            if let Some(pilot) = pilot {
                let change = attribute_change(pilot, Value::Int(ship), ATTR_CHARGE, Value::Float(debit.after), Value::Float(debit.before));
                self.notify_pilot(pilot, "OnModuleAttributeChanges", vec![Value::list([change])]);
            }
        }
        if debit.drained == 0.0 {
            self.stop_unpaid_warp(ship, stamp);
        } else if debit.drained < cost {
            let shortened = add(pos, scale(sub(endpoint, pos), debit.drained / cost));
            self.balls.get_mut(&ship).unwrap().activate_warp_to(stamp, shortened);
            let intent = self.warp_exits.get_mut(&ship).unwrap();
            intent.pending = false;
            intent.full = false;
            let arrival = intent.arrival.take();
            if let Some(id) = arrival { self.release_warp_arrival(id); }
            self.warp_corrections.push(ship);
        } else {
            self.balls.get_mut(&ship).unwrap().activate_warp_to(stamp, endpoint);
            self.warp_exits.get_mut(&ship).unwrap().pending = false;
        }
    }

    fn stop_unpaid_warp(&mut self, ship: i64, stamp: i64) {
        self.cancel_warp_intent(ship);
        self.apply_command(ship, stamp, |ball, stamp, _| {
            ball.stop();
            vec![event(stamp, "Stop", vec![Value::Int(ship)])]
        });
    }

    pub(super) fn broadcast_warp_corrections(&mut self, stamp: i64, collisions: bool) {
        for index in 0..self.warp_corrections.len() {
            let ship = self.warp_corrections[index];
            if let Some(ball) = self.balls.get(&ship) {
                let visibility = (ball.id, ball.cloak);
                let update = add_balls_event(stamp, ball, collisions);
                self.deliver(Some(visibility), vec![update]);
            }
        }
    }
}

#[cfg(test)] mod tests;
