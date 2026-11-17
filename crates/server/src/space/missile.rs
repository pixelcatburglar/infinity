//! Native missile launch setup and predictive steering for transient balls.
//! Mirrors destiny LaunchMissile, MissileFollow and EvolveMissile; the client
//! `eve/client/script/remote/michelle.py` consumes LaunchMissile and serialized missile mode.
//! Data comes from actor-owned Ball and target snapshots; missiles are transient, with no DB tables.
//! Flight lifetime and target-only contact sweeps live in `system::missiles`.
//! Dynamic orientation is unsupported: a stationary launcher uses default -Z.

use super::*;

/// Minimum launcher fallback speed in m/s (native ThunkersPyLaunchMissile:925).
const MIN_LAUNCHER_FALLBACK_SPEED_M_S: f64 = 150.0;
/// Initial absolute missile GotoReach in metres (native BallparkMissileFollow:3867).
const INITIAL_MISSILE_GOTO_REACH_M: f64 = 1e16;
/// Initial straight launch window in milliseconds (native EvolveMissile:974).
const INITIAL_STRAIGHT_LAUNCH_WINDOW_MS: f64 = 800.0;

impl Ball {
    pub(super) fn launch_native_missile(&mut self, launcher: (V3, V3, f32), target: Option<(i64, V3, f32, u8)>, owner: i64, stamp: i64) {
        let aimed = target.filter(|t| t.0 != owner && t.3 == 0);
        let direction = if let Some(t) = aimed {
            norm(sub(t.1, launcher.0))
        } else {
            let heading = norm(launcher.1);
            let heading = if heading == [0.0; 3] { [0.0, 0.0, -1.0] } else { heading };
            add(launcher.1, scale(heading, (launcher.2 as f64).max(MIN_LAUNCHER_FALLBACK_SPEED_M_S)))
        };
        self.pos = launcher.0;
        self.old_pos = self.pos;
        self.vel = direction;
        self.old_vel = self.vel;
        self.segment_acceleration = None;
        if target.is_some_and(|t| t.3 != 0) { return; }
        if let Some(t) = aimed {
            self.missile_follow(t.0, t.2, owner, stamp);
        } else if launcher.2 > 0.0 {
            self.goto_direction(self.vel);
        }
    }

    fn missile_follow(&mut self, target: i64, radius: f32, owner: i64, stamp: i64) {
        if self.id == target || self.id == owner || target == owner { return; }
        self.stop();
        self.follow_id = target;
        self.follow_range = -(self.radius + radius);
        self.launcher = owner;
        self.effect_stamp = stamp;
        self.goto = scale(norm(self.vel), INITIAL_MISSILE_GOTO_REACH_M);
        self.speed_fraction = 1.0;
        self.mode = MODE_MISSILE;
    }

    pub(super) fn evolve_missile(&mut self, target: &TargetState, current_time: i64) -> V3 {
        if seconds_to_ms_f64((current_time - self.effect_stamp) as f64) <= INITIAL_STRAIGHT_LAUNCH_WINDOW_MS {
            return self.goto_thrust(self.goto, false);
        }
        let delta = sub(self.pos, target.pos);
        let distance = len(delta);
        let radius = self.follow_range as f64 + self.radius as f64 + target.radius as f64;
        self.goto = if distance == 0.0 {
            add(target.pos, [radius, 0.0, 0.0])
        } else {
            let predicted = add(target.pos, scale(target.vel, distance / self.max_vel as f64));
            add(predicted, scale(scale(delta, radius), 1.0 / distance))
        };
        self.goto_thrust(self.goto, true)
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
