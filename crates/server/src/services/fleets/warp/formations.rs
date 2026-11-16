//! Deterministic fleet-warp arrival layouts: one nominal endpoint per participant, planned
//! from actor-supplied position samples before any per-member admission runs. World up is
//! +Y; Plane stays world-horizontal, Wall faces the incoming direction and Arrow fills a
//! 90-degree wedge behind its tip. Object clearance translates the whole formation along
//! the outward axis by the smallest safe amount. No database, persistence, follow mode or
//! per-pilot retries live here.
use super::settings::{FormationKind, FormationSettings};
use crate::services::{rand01, rand_dir};
use crate::space::{self, V3};
use crate::space::system::warp::{ordinary_warp_standoff, WarpDestination};

/// Approved random-radius policy: Point spreads members through a ball of this radius.
const POINT_RADIUS_M: f64 = 2_500.0;
/// Client evefleet.const: Relative joins members within this distance of the commander.
const RELATIVE_RANGE_M: f64 = 200_000.0;
/// World up direction (eve/client/script/ui/camera/baseCamera.py `default_upDirection`).
const UP: V3 = [0.0, 1.0, 0.0];
/// Squared-norm cutoff for the up projection: below it `f` is numerically parallel to up.
const DEGENERATE2: f64 = f64::EPSILON;
/// Server-chosen deterministic fallback axis when `f` is parallel to world up.
const X: V3 = [1.0, 0.0, 0.0];

/// Server policy: Sphere's selected size is a diameter, so the ball radius is half of it.
const DIAMETER_PER_RADIUS: f64 = 2.0;

#[derive(Clone, Copy)]
pub(super) struct MemberSnapshot {
    pub(super) pilot: i64,
    pub(super) ship: i64,
    pub(super) pos: V3,
    pub(super) radius: f64,
}
pub(super) struct PlannedPilot {
    pub(super) pilot: i64,
    pub(super) ship: i64,
    /// World-coordinate nominal endpoint handed to `warp_pilot` as `Exact`.
    pub(super) endpoint: V3,
    /// Hull radius for object clearance only; never reaches admission.
    radius: f64,
}

fn finite(m: &MemberSnapshot) -> bool {
    m.radius.is_finite() && m.pos.iter().all(|v| v.is_finite())
}
fn push(out: &mut Vec<PlannedPilot>, m: MemberSnapshot, rel: V3) {
    out.push(PlannedPilot { pilot: m.pilot, ship: m.ship, endpoint: rel, radius: m.radius });
}
/// Arrival-plane basis: `r` right, `u` up, both orthogonal to `f`.
fn frame(f: V3) -> (V3, V3) {
    let mut v = space::sub(UP, space::scale(f, space::dot(UP, f)));
    if space::dot(v, v) <= DEGENERATE2 {
        // `f` nearly parallel to world up: project the positive axis least parallel to it.
        let axis = [X, UP, [0.0, 0.0, 1.0]].into_iter()
            .min_by(|a, b| space::dot(*a, f).abs().total_cmp(&space::dot(*b, f).abs())).unwrap_or(X);
        v = space::sub(axis, space::scale(f, space::dot(axis, f)));
    }
    let u = space::norm(v);
    let r = space::norm(space::cross(f, u));
    (r, space::norm(space::cross(r, f)))
}
/// Plane basis: `h` forward in the world XZ plane, `r_h` right of it.
fn plane_frame(f: V3) -> (V3, V3) {
    let flat = [f[0], 0.0, f[2]];
    let h = if space::dot(flat, flat) == 0.0 { X } else { space::norm(flat) };
    (space::norm(space::cross(h, UP)), h)
}
/// One Chebyshev ring of (i, j) slots, clockwise from (0, k); yields exactly 8k points.
fn square_ring(k: i64) -> impl Iterator<Item = (i64, i64)> {
    (0..=k).map(move |i| (i, k))
        .chain((-k..k).rev().map(move |j| (k, j)))
        .chain((-k..k).rev().map(move |i| (i, -k)))
        .chain((-k + 1..=k).map(move |j| (-k, j)))
        .chain((-k + 1..0).map(move |i| (i, k)))
}
/// One Arrow row of (k, i) slots: row centre first, then alternating flanks.
fn arrow_row(k: i64) -> impl Iterator<Item = (i64, i64)> {
    std::iter::once(0).chain((1..=k).flat_map(|i| [-i, i])).map(move |i| (k, i))
}
/// Uniform-in-volume radius for a [0,1) draw inside a ball of `radius`.
fn ball_radius(radius: f64, draw: f64) -> f64 { radius * draw.cbrt() }

/// Plan every participant's nominal endpoint. `members` yields candidates in fleet order,
/// commander last when they take part. Returns `None` on non-finite geometry so no invalid
/// coordinate reaches the route; an empty candidate set yields `Some(vec![])`.
pub(super) fn plan(
    settings: FormationSettings, destination: WarpDestination,
    commander: MemberSnapshot, members: impl Iterator<Item = MemberSnapshot>,
) -> Option<Vec<PlannedPilot>> {
    let (point, object_radius, min_range) = match destination {
        WarpDestination::Ordinary { point, object_radius, min_range } => (point, object_radius, min_range),
        WarpDestination::Exact(point) => (point, None, 0.0),
    };
    let to_target = space::sub(point, commander.pos);
    let f = if space::dot(to_target, to_target) == 0.0 { X } else { space::norm(to_target) };
    if !point.iter().all(|v| v.is_finite()) || !f.iter().all(|v| v.is_finite()) || !commander.radius.is_finite() {
        return None;
    }
    let outward = space::scale(f, -1.0);
    // Nominal anchor and all slot offsets are staged relative to the target point;
    // world translation happens once at the end.
    let anchor = space::scale(outward, ordinary_warp_standoff(object_radius, commander.radius, min_range));
    let (low, high) = members.size_hint();
    let mut planned: Vec<PlannedPilot> = Vec::with_capacity(high.unwrap_or(low));
    match settings.kind {
        FormationKind::Point | FormationKind::Sphere => {
            let radius = if settings.kind == FormationKind::Point { POINT_RADIUS_M } else { settings.size_m / DIAMETER_PER_RADIUS };
            for m in members {
                if !finite(&m) { return None; }
                push(&mut planned, m, space::add(anchor, space::scale(rand_dir(), ball_radius(radius, rand01()))));
            }
        }
        FormationKind::Plane | FormationKind::Wall => {
            let d = settings.spacing_m;
            let (r, u) = if settings.kind == FormationKind::Plane { plane_frame(f) } else { frame(f) };
            let mut slots = (1i64..).flat_map(square_ring);
            for m in members {
                if !finite(&m) { return None; }
                let (i, j) = if m.pilot == commander.pilot { (0, 0) } else { slots.next().unwrap_or((0, 0)) };
                let offset = space::add(space::scale(r, i as f64 * d), space::scale(u, j as f64 * d));
                push(&mut planned, m, space::add(anchor, offset));
            }
        }
        FormationKind::Arrow => {
            let d = settings.spacing_m;
            let (r, _) = frame(f);
            let mut rows = (1i64..).flat_map(arrow_row);
            for m in members {
                if !finite(&m) { return None; }
                let (k, i) = if m.pilot == commander.pilot { (0, 0) } else { rows.next().unwrap_or((0, 0)) };
                let offset = space::add(space::scale(f, -(k as f64) * d), space::scale(r, i as f64 * d));
                push(&mut planned, m, space::add(anchor, offset));
            }
        }
        FormationKind::Relative => {
            for m in members {
                if !finite(&m) { return None; }
                if space::dist(m.pos, commander.pos) > RELATIVE_RANGE_M { continue; }
                push(&mut planned, m, space::add(anchor, space::sub(m.pos, commander.pos)));
            }
        }
    }
    // Minimal rigid translation along `outward` so every endpoint clears the object.
    if object_radius.is_some() && planned.iter().any(|p| {
        let c = ordinary_warp_standoff(object_radius, p.radius, min_range);
        space::dot(p.endpoint, p.endpoint) < c * c
    }) {
        let mut intervals: Vec<(f64, f64)> = Vec::with_capacity(planned.len());
        intervals.extend(planned.iter().filter_map(|p| {
            let c = ordinary_warp_standoff(object_radius, p.radius, min_range);
            let b = space::dot(p.endpoint, outward);
            let perp = space::sub(p.endpoint, space::scale(outward, b));
            let perp2 = space::dot(perp, perp);
            if c <= 0.0 || perp2 >= c * c { return None; }
            let root = (c * c - perp2).sqrt();
            (-b + root > 0.0).then_some((-b - root, -b + root))
        }));
        intervals.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let mut lambda = 0.0;
        for (lo, hi) in intervals {
            if hi <= lambda { continue; }
            if lo >= lambda { break; }
            lambda = hi;
        }
        if lambda > 0.0 {
            let shift = space::scale(outward, lambda);
            for p in &mut planned { p.endpoint = space::add(p.endpoint, shift); }
        }
    }
    for p in &mut planned { p.endpoint = space::add(p.endpoint, point); }
    if planned.iter().any(|p| !p.endpoint.iter().all(|v| v.is_finite())) { return None; }
    Some(planned)
}

#[cfg(all(test, feature = "tests-social"))]
#[path = "formations/tests.rs"]
mod tests;
