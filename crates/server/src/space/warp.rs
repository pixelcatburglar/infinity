//! Native warp alignment, motion and interpolation for transient destiny balls.
//! Mirrors carbonengine Ballpark::WarpTo, EvolveWarp and WarpDistance and
//! Ball::IsAlignedForWarp, consumed by the client destiny ballpark. Ball state
//! is transient; warp sensors are not implemented.

use super::*;

/// Ballpark::WarpTo minimum raw distance, in metres.
const MIN_WARP_DISTANCE_SQUARED: f64 = 10_000_000_000.0;
/// Ball::IsAlignedForWarp maximum direction error.
const ALIGN_DIRECTION_ERROR: f64 = 0.01;
/// Ball::IsAlignedForWarp squared minimum speed fraction.
const ALIGN_SPEED_SQUARED: f64 = 0.5625;
/// Ball::IsAlignedForWarp forced alignment deadline.
const MAX_ALIGN_TICKS: i64 = 180;
/// Ballpark.cpp warp factor conversions and deceleration cap.
const FACTOR_TO_AU_PER_SECOND: f64 = 0.001;
/// Ballpark.cpp SetupWarpConstants warp factor to acceleration rate.
const FACTOR_TO_ACCELERATION: f64 = 1.0 / 1000.0;
/// Ballpark.cpp SetupWarpConstants warp factor to deceleration rate.
const FACTOR_TO_DECELERATION: f64 = 1.0 / 3000.0;
/// Ballpark.cpp SetupWarpConstants maximum deceleration rate.
const MAX_DECELERATION: f64 = 2.0;
/// Ballpark::WarpDistance exit speed cap in metres per second.
const EXIT_SPEED_CAP: f64 = 100.0;

#[derive(Clone)]
struct WarpProfile {
    accel_duration: f64, cruise_duration: f64, decel_duration: f64,
    accel_distance: f64, cruise_distance: f64, decel_distance: f64,
    accel_rate: f64, decel_rate: f64, speed: f64,
}

impl WarpProfile {
    fn setup_warp_constants(factor: i64, distance: f64) -> Self {
        let factor = factor as f64;
        let accel_rate = factor * FACTOR_TO_ACCELERATION;
        let decel_rate = (factor * FACTOR_TO_DECELERATION).min(MAX_DECELERATION);
        let speed = (factor * FACTOR_TO_AU_PER_SECOND * AU)
            .min((distance + 1.0) * accel_rate * decel_rate / (accel_rate + decel_rate));
        let accel_duration = (speed / accel_rate).ln() / accel_rate;
        let cruise_duration = distance / speed - 1.0 / accel_rate - 1.0 / decel_rate + 1.0 / speed;
        let decel_duration = (speed / decel_rate).ln() / decel_rate;
        let accel_distance = speed / accel_rate;
        let cruise_distance = speed * cruise_duration;
        let decel_distance = speed / decel_rate - 1.0;
        Self { accel_duration, cruise_duration, decel_duration, accel_distance,
            cruise_distance, decel_distance, accel_rate, decel_rate, speed }
    }
}

/// Persistent native warp stream fields; length survives Stop and later alignment.
#[derive(Clone)]
pub struct WarpState {
    pub length: f64,
    pub min_range: f64,
    pub factor: i64,
    profile: Option<WarpProfile>,
}

impl Default for WarpState {
    fn default() -> Self { Self { length: -1.0, min_range: 0.0, factor: 0, profile: None } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpOutcome { Ignored, Approach, Align }

impl Ball {
    /// Convert AU/s to the integer milli-AU/s carried by destiny.
    pub(crate) fn warp_factor_from_speed(speed_au_per_second: f64) -> Option<i64> {
        if !speed_au_per_second.is_finite() || speed_au_per_second <= 0.0 { return None; }
        let factor = (speed_au_per_second / FACTOR_TO_AU_PER_SECOND).round();
        if !factor.is_finite() || factor >= i64::MAX as f64 { return None; }
        Some(factor as i64)
    }

    /// Initialize a native WarpTo request without changing throttle.
    pub fn warp_to(&mut self, point: V3, min_range: f64, factor: i64) -> WarpOutcome {
        if !point.iter().all(|v| v.is_finite()) { return WarpOutcome::Ignored; }
        let delta = sub(self.pos, point);
        let distance2 = delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2];
        if distance2 < MIN_WARP_DISTANCE_SQUARED {
            self.goto_point(point);
            return WarpOutcome::Approach;
        }
        if self.mode == MODE_WARP && point == self.goto { return WarpOutcome::Ignored; }
        self.stop();
        self.goto = point;
        self.effect_stamp = -1;
        self.follow_range = 0.0;
        self.warp.min_range = if min_range < 0.0 { 0.0 } else { min_range };
        self.warp.factor = factor.max(1);
        self.warp.profile = None;
        self.mode = MODE_WARP;
        WarpOutcome::Align
    }

    pub fn is_warping(&self) -> bool { self.mode == MODE_WARP && self.effect_stamp >= 0 }

    pub(super) fn is_aligned_for_warp(&self) -> bool {
        if self.mode != MODE_WARP || self.effect_stamp >= 0 { return false; }
        let direction = norm(sub(self.goto, self.pos));
        let velocity = norm(self.vel);
        let dot = velocity[0] * direction[0] + velocity[1] * direction[1] + velocity[2] * direction[2];
        let speed2 = self.vel[0] * self.vel[0] + self.vel[1] * self.vel[1] + self.vel[2] * self.vel[2];
        ((1.0 - dot).abs() < ALIGN_DIRECTION_ERROR
            && speed2 > ALIGN_SPEED_SQUARED * self.max_vel as f64 * self.max_vel as f64)
            || self.effect_stamp < -MAX_ALIGN_TICKS
    }

    pub(super) fn evolve_warp(&mut self, current_time: i64) -> V3 {
        if self.is_warping() { return V3::default(); }
        if self.is_aligned_for_warp() {
            self.real_warp(current_time);
            V3::default()
        } else {
            self.effect_stamp -= 1;
            self.evolve_goto()
        }
    }

    /// Ballpark::RealWarp activates an aligned warp.
    fn real_warp(&mut self, current_time: i64) {
        self.activate_warp_to(current_time, self.resolved_warp_endpoint());
    }

    pub(super) fn resolved_warp_endpoint(&self) -> V3 {
        add(self.goto, scale(norm(sub(self.pos, self.goto)), self.warp.min_range))
    }

    pub(super) fn activate_warp_to(&mut self, current_time: i64, endpoint: V3) {
        self.goto = endpoint;
        self.effect_stamp = current_time;
        self.warp.length = len(sub(self.goto, self.pos));
        self.warp.profile = Some(WarpProfile::setup_warp_constants(self.warp.factor, self.warp.length));
        self.massive = false;
    }

    pub(super) fn warp_distance(&self, mut pos: V3, mut vel: V3, t: f64, interpolating: bool) -> (V3, V3, bool) {
        let fallback;
        let p = match &self.warp.profile {
            Some(profile) => profile,
            None => { fallback = WarpProfile::setup_warp_constants(self.warp.factor, self.warp.length); &fallback }
        };
        let direction = norm(sub(self.goto, pos));
        let distance;
        let mut exit = false;
        if t < p.accel_duration {
            let speed = (p.accel_rate * (p.accel_rate * t).exp()).max(len(vel));
            vel = scale(direction, speed);
            distance = (p.accel_rate * t).exp();
        } else if t - p.accel_duration < p.cruise_duration {
            vel = scale(direction, p.speed);
            distance = p.speed * (t - p.accel_duration) + p.accel_distance;
        } else if t - p.cruise_duration - p.accel_duration < p.decel_duration + 1.0 {
            let speed = p.speed * (-p.decel_rate * (t - p.cruise_duration - p.accel_duration)).exp();
            vel = scale(direction, speed);
            distance = p.speed / p.decel_rate - (p.speed / p.decel_rate)
                * (-p.decel_rate * (t - p.cruise_duration - p.accel_duration)).exp()
                + p.accel_distance + p.cruise_distance;
            exit = !interpolating && speed < (self.max_vel as f64 / 2.0).min(EXIT_SPEED_CAP);
        } else {
            tracing::error!(ball = self.id, factor = self.warp.factor, length = self.warp.length, t, interpolating, "ship stuck in extended warp");
            return (self.goto, V3::default(), !interpolating);
        }
        pos = sub(self.goto, scale(direction, p.accel_distance + p.cruise_distance + p.decel_distance - distance));
        (pos, vel, exit)
    }
}

#[cfg(test)] mod tests;
