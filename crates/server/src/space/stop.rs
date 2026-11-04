//! Native STOP behavior and motion setters used by destiny commands.
//! Mirrors carbonengine destiny `Ballpark.cpp` Stop, EvolveStop and property setters,
//! as consumed by `eve/client/script/remote/michelle.py`; ball state is transient.
//! Formation followers and dynamical orientation are not implemented.

use super::{Ball, MODE_FOLLOW, MODE_MISSILE, MODE_ORBIT, MODE_STOP};

/// Ballpark.cpp EvolveStop vertical velocity damping.
const STOP_VERTICAL_DAMPING: f64 = 0.07;
/// Ballpark.cpp EvolveStop vertical velocity retention.
const STOP_VERTICAL_RETAIN: f64 = 0.9345794392523364485981308411215;

impl Ball {
    /// Stop without changing throttle or directly zeroing velocity.
    pub fn stop(&mut self) {
        if matches!(self.mode, MODE_FOLLOW | MODE_ORBIT | MODE_MISSILE) {
            self.effect_stamp = 0;
            self.follow_id = 0;
            self.launcher = 0;
            self.follow_range = 0.0;
        }
        self.mode = MODE_STOP;
    }

    pub(super) fn evolve_stop(&mut self) {
        self.vel[1] = (self.vel[1] - STOP_VERTICAL_DAMPING * self.vel[1]) * STOP_VERTICAL_RETAIN;
    }

    /// Ballpark::SetSpeedFraction; invalid input leaves the ball unchanged.
    pub fn set_speed_fraction(&mut self, fraction: f32) {
        if fraction.is_finite() { self.speed_fraction = fraction.clamp(0.0, 1.0); }
    }

    pub(crate) fn valid_agility(value: f64) -> bool {
        value.is_finite() && (value as f32).is_finite() && (value as f32) > 0.0
    }

    pub(crate) fn valid_mass(value: f64) -> bool { value.is_finite() && value > 0.0 }

    pub(crate) fn valid_max_speed(value: f64) -> bool {
        value.is_finite() && (value as f32).is_finite() && value >= 0.0
    }

    /// Ballpark::SetMaxSpeed, with finite validation at the server boundary.
    pub fn set_max_speed(&mut self, speed: f64) -> bool {
        if !Self::valid_max_speed(speed) { return false; }
        self.max_vel = speed as f32;
        true
    }

    /// Ballpark::SetBallMass, with finite validation at the server boundary.
    pub fn set_ball_mass(&mut self, mass: f64) -> bool {
        if !Self::valid_mass(mass) { return false; }
        self.mass = mass;
        true
    }

    /// Ballpark::SetBallAgility, with finite validation at the server boundary.
    pub fn set_ball_agility(&mut self, agility: f64) -> bool {
        if !Self::valid_agility(agility) { return false; }
        self.agility = agility as f32;
        true
    }
}

#[cfg(test)] mod tests;
