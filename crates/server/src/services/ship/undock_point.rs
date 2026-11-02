//! NPC station exit placement and initial ship movement.
//! Mirrors `ballparkCommon/docking.py` using immutable graphic locators; NPC
//! station positions have no `dunRotation`. If added, rotate both locator offset
//! and channel here together. Upwell-specific behavior is not modeled.
use crate::consts::*;
use crate::sde::graphic_locations::DirectionalLocator;
use crate::space::{add, dist, scale, sub, Ball, V3};

/// `ballparkCommon/docking.py`: angular jitter in radians.
const JITTER: f64 = 0.25;
/// `ballparkCommon/docking.py`: initial fitted-speed multiplier.
const VELOCITY_MULTIPLIER: f64 = 1.75;
/// `ballparkCommon/docking.py`: initial speed cap in metres per second.
const VELOCITY_MAXIMUM: f64 = 700.0;
/// Existing server undock placement clearance policy in metres.
const CLEARANCE: f64 = 2500.0;
/// `eve/common/lib/appConst.py`: SMALL_UNDOCKPOINT_CATEGORY.
const SMALL: &str = "undockPointSmall";
/// `eve/common/lib/appConst.py`: LARGE_UNDOCKPOINT_CATEGORY.
const LARGE: &str = "undockPointLarge";
/// `eve/common/lib/appConst.py`: CAPITAL_UNDOCKPOINT_CATEGORY.
const CAPITAL: &str = "undockPointCapital";
/// `eve/common/lib/appConst.py`: SUPERCAPITAL_UNDOCKPOINT_CATEGORY.
const SUPERCAPITAL: &str = "undockPointSupercapital";

fn category(group: Option<i32>) -> &'static str {
    match group {
        Some(GROUP_FREIGHTER | GROUP_INDUSTRIAL | GROUP_TRANSPORT_SHIP | GROUP_BLOCKADE_RUNNER |
            GROUP_JUMP_FREIGHTER | GROUP_BATTLESHIP | GROUP_ELITE_BATTLESHIP | GROUP_BLACK_OPS |
            GROUP_MARAUDERS | GROUP_INDUSTRIAL_COMMAND_SHIP) => LARGE,
        Some(GROUP_CARRIER | GROUP_COMMAND_CARRIER | GROUP_FORCE_AUX | GROUP_DREADNOUGHT |
            GROUP_LANCER_DREADNOUGHT | GROUP_CAPITAL_INDUSTRIAL_SHIP) => CAPITAL,
        Some(GROUP_TITAN | GROUP_SUPERCARRIER) => SUPERCAPITAL,
        _ => SMALL,
    }
}

fn select<'a>(locators: &'a [DirectionalLocator], group: Option<i32>, rng: &mut impl FnMut() -> f64) -> Option<&'a DirectionalLocator> {
    let category = category(group);
    let matching = locators.iter().filter(|l| l.category == category).count();
    let count = if matching == 0 { locators.len() } else { matching };
    if count == 0 { return None; }
    let index = ((rng() * count as f64) as usize).min(count - 1);
    if matching == 0 { locators.get(index) } else { locators.iter().filter(|l| l.category == category).nth(index) }
}

fn normalize(v: V3) -> V3 {
    let largest = v.iter().fold(0.0_f64, |a, x| a.max(x.abs()));
    let v = [v[0] / largest, v[1] / largest, v[2] / largest];
    let length = v[0].hypot(v[1]).hypot(v[2]);
    scale(v, 1.0 / length)
}

fn position(center: V3, offset: V3, direction: V3, radius: f64, collisions: bool) -> V3 {
    let p0 = add(center, offset);
    if !collisions || dist(p0, center) >= radius { return p0; }
    let relative = sub(p0, center);
    let projection = relative[0] * direction[0] + relative[1] * direction[1] + relative[2] * direction[2];
    let squared = relative.iter().map(|x| x * x).sum::<f64>();
    let mut t = -projection + (projection * projection + radius * radius - squared).sqrt();
    let mut point = add(p0, scale(direction, t));
    let mut extra = center.iter().chain(p0.iter()).fold(radius, |a, x| a.max(x.abs())) * f64::EPSILON;
    while dist(point, center) < radius {
        t += extra;
        extra *= 2.0;
        point = add(p0, scale(direction, t));
    }
    point
}

// D3DXQuaternionRotationYawPitchRoll: roll, then pitch, then yaw.
fn rotate(v: V3, yaw: f64, pitch: f64, roll: f64) -> V3 {
    let (sy, cy) = (yaw * 0.5).sin_cos();
    let (sp, cp) = (pitch * 0.5).sin_cos();
    let (sr, cr) = (roll * 0.5).sin_cos();
    let q = [cy * sp * cr + sy * cp * sr, sy * cp * cr - cy * sp * sr,
        cy * cp * sr - sy * sp * cr];
    let w = cy * cp * cr + sy * sp * sr;
    let cross = |a: V3, b: V3| [a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]];
    let t = scale(cross(q, v), 2.0);
    add(v, add(scale(t, w), cross(q, t)))
}

pub(super) fn prepare(ball: &mut Ball, locators: &[DirectionalLocator], group: Option<i32>, center: V3, station_radius: f32, collisions: bool, mut rng: impl FnMut() -> f64) {
    let (offset, direction) = select(locators, group, &mut rng)
        .map(|l| (l.position, normalize(l.direction))).unwrap_or(([0.0; 3], [0.0, 0.0, 1.0]));
    ball.pos = position(center, offset, direction, f64::from(station_radius) + f64::from(ball.radius) + CLEARANCE, collisions);
    ball.old_pos = ball.pos;
    let mut angle = || (rng() * 2.0 - 1.0) * JITTER;
    let direction = normalize(rotate(direction, angle(), angle(), angle()));
    ball.goto_direction(direction);
    ball.speed_fraction = 1.0;
    ball.vel = scale(direction, (VELOCITY_MULTIPLIER * f64::from(ball.max_vel)).min(VELOCITY_MAXIMUM));
    ball.old_vel = ball.vel;
    ball.segment_acceleration = None;
}

#[cfg(test)] mod tests;
