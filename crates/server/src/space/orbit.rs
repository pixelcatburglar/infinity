//! Native old-style orbit steering (Ballpark.cpp 1246-1328), consumed by
//! the client through remote/michelle.py. State and target snapshots are transient,
//! not database-backed. New-style orbit and dynamical orientation are unsupported.
use super::{add, cross, len, norm, scale, sub, Ball, TargetState, V3, AU, DESTINY_FRICTION, MODE_ORBIT};

/// Precession from carbon engine Ballpark.cpp EvolveOldStyleOrbit/ORBITAL_PRECESSION.
const PRECESSION: f64 = 0.001;
/// Precision for EvolveOldStyleOrbit's seven-decimal component and radial-factor truncation.
const PLANE_PRECISION: f64 = 10_000_000.0;
/// Denominator of 40000 in EvolveOldStyleOrbit's radialFactor.
const RADIAL_FALLOFF: f64 = 40_000.0;
/// The 10 AU goto projection factor from EvolveOldStyleOrbit.
const GOTO_REACH_AU: f64 = 10.0;

impl Ball {
    /// Begin orbiting a visible distinct target at a finite range.
    pub fn orbit(&mut self, target: &Ball, range: f32) -> bool {
        if !range.is_finite() || target.id == self.id || target.cloak != 0 { return false; }
        self.stop();
        self.follow_id = target.id;
        self.follow_range = range;
        self.mode = MODE_ORBIT;
        true
    }

    fn default_orbital_normal(&self, current_time: i64) -> V3 {
        let phi1 = current_time as f64 * PRECESSION;
        let phi2 = (self.id & 0xffff) as f64 + current_time as f64 * PRECESSION;
        [phi1.cos() * phi2.cos(), phi2.sin(), phi1.sin() * phi2.cos()]
            .map(|v| ((v * PLANE_PRECISION) as i64) as f64 / PLANE_PRECISION)
    }

    pub(super) fn evolve_old_style_orbit(&mut self, target: &TargetState, current_time: i64) -> V3 {
        let cruise = self.speed_fraction as f64 * self.max_vel as f64;
        let max_thrust = DESTINY_FRICTION * cruise / (self.mass * self.agility as f64);
        let radius = self.follow_range as f64 + self.radius as f64 + target.radius as f64;
        let mut to = sub(target.pos, self.pos);
        let distance = len(to);
        to = norm(to);
        let radial = norm(cross(self.default_orbital_normal(current_time), to));
        let to_component = distance * distance - radius * radius;
        if to_component >= 0.0 {
            let radial_component = radius * to_component.sqrt() / distance;
            to = norm(add(scale(to, to_component / distance), scale(radial, radial_component)));
        }
        let difference = radius - distance;
        let radial_factor = ((-(difference * difference) / RADIAL_FALLOFF).exp() * PLANE_PRECISION) as i64 as f64 / PLANE_PRECISION;
        let phi1 = (-to[0]) * radial[0] + (-to[1]) * radial[1] + (-to[2]) * radial[2];
        let transverse = 1.0 + radial_factor * radial_factor * (phi1 * phi1 - 1.0);
        let transverse = (if transverse > 0.0 {
            radial_factor * phi1 + transverse.sqrt()
        } else {
            radial_factor * phi1
        }) * if distance - radius >= 0.0 { 1.0 } else { -1.0 };
        let acceleration = scale(add(scale(radial, radial_factor), scale(to, transverse)), max_thrust);
        self.goto = add(self.pos, scale(acceleration, GOTO_REACH_AU * AU));
        acceleration
    }
}

#[cfg(all(test, feature = "tests-space"))] mod tests;
