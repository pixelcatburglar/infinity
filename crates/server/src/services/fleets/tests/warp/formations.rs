//! Shared plan/commit freeze tests: one position sample, reserved commander
//! slots and unchanged later endpoints after member refusal.
use super::*;
use settings::{FormationKind, FormationSettings};
use crate::services::ship::{commit_warp, WarpCommit};

fn formation(kind: FormationKind) -> FormationSettings {
    FormationSettings { kind, spacing_m: 1000.0, size_m: 10_000.0 }
}
fn placed(id: i64, pilot: i64, x: f64) -> space::Ball {
    let mut b = ball(id, pilot);
    b.pos = [x, 0.0, 0.0];
    b
}

#[test]
fn prepare_relative_offsets_use_one_actor_sample() {
    crate::tests::init_config();
    let st = setup();
    let mut order = snapshot(&st, 1, 101).unwrap();
    order.settings = formation(FormationKind::Relative);
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    for (id, pilot, x) in [(101, 1, 0.0), (103, 3, 200_000.0), (104, 4, 200_000.1)] { s.insert(placed(id, pilot, x)); }
    let mut moving = placed(102, 2, 100_000.0);
    moving.mode = space::MODE_GOTO;
    moving.goto = [200_000.0, 0.0, 0.0];
    moving.speed_fraction = 1.0;
    s.insert(moving);
    s.tick(2000);
    s.tick(2500);
    let now = s.now_ms();
    let (pc, p2) = (s.ball(101).unwrap().position_at(now), s.ball(102).unwrap().position_at(now));
    assert_ne!(p2, s.ball(102).unwrap().pos, "sample must read the interpolated segment");
    let point = [9_999_000.0, 0.0, 0.0];
    let planned = prepare(&s, &order, WarpDestination::Ordinary { point, object_radius: None, min_range: 0.0 }).unwrap();
    assert_eq!(planned.iter().map(|p| p.pilot).collect::<Vec<_>>(), vec![2, 3, 1], "boundary is inclusive");
    assert!(space::dist(planned[0].endpoint, space::add(point, space::sub(p2, pc))) < 1e-6);
    assert_eq!(planned[1].endpoint, [9_999_000.0 + 200_000.0, 0.0, 0.0]);
    assert_eq!(planned[2].endpoint, point);
}

#[test]
fn prepare_leaves_commander_plane_centre_empty() {
    crate::tests::init_config();
    let mut st = setup();
    st.fleets.get_mut(&100).unwrap().members.get_mut(&3).unwrap().opt_outs.accepts_fleet_warp = false;
    let mut order = snapshot(&st, 3, 103).unwrap();
    order.settings = formation(FormationKind::Plane);
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    s.insert(ball(103, 3));
    s.insert(ball(104, 4));
    let planned = prepare(&s, &order, WarpDestination::Ordinary { point: [9_999_000.0, 0.0, 0.0], object_radius: None, min_range: 0.0 }).unwrap();
    // f = +X, so ring slot (0,1) lands at A + forward * spacing, not the centre.
    assert_eq!(planned.len(), 1);
    assert_eq!((planned[0].pilot, planned[0].endpoint), (4, [10_000_000.0, 0.0, 0.0]));
}

#[test]
fn refused_member_does_not_replan_later_endpoints() {
    crate::tests::init_config();
    let mut st = setup();
    let mut order = snapshot(&st, 2, 102).unwrap();
    order.settings = formation(FormationKind::Sphere);
    let mut s = space::System::new(1, std::sync::Arc::default(), 1000);
    for (pilot, ship) in [(2, 102), (3, 103), (4, 104), (5, 105)] { s.insert(ball(ship, pilot)); }
    let planned = prepare(&s, &order, WarpDestination::Ordinary { point: [9_999_000.0, 0.0, 0.0], object_radius: None, min_range: 0.0 }).unwrap();
    assert_eq!(planned.len(), 4);
    let endpoint = |p: i64| planned.iter().find(|e| e.pilot == p).unwrap().endpoint;
    // Member 4 opts out after planning; member 3 sees one stale projection retry.
    st.fleets.get_mut(&100).unwrap().members.get_mut(&4).unwrap().opt_outs.accepts_fleet_warp = false;
    assert!(commit_in(&st, &mut s, &order, 4, 104, |_| ()).is_none());
    let stale = s.projection_revision(103);
    s.refresh_projection_target(103);
    let commit = |st: &State, s: &mut space::System, pilot: i64, ship: i64, revision: u64| {
        commit_in(st, s, &order, pilot, ship, |s| commit_warp(s, ship, revision, false, 3000, WarpDestination::Exact(endpoint(pilot)), None)).unwrap()
    };
    assert_eq!(commit(&st, &mut s, 3, 103, stale), WarpCommit::RetryProjection);
    let (r3, r5) = (s.projection_revision(103), s.projection_revision(105));
    assert_eq!(commit(&st, &mut s, 3, 103, r3), WarpCommit::Queued);
    assert_eq!(commit(&st, &mut s, 5, 105, r5), WarpCommit::Queued);
    s.tick(2000);
    assert_eq!(s.ball(103).unwrap().goto, endpoint(3));
    assert_eq!(s.ball(105).unwrap().goto, endpoint(5));
    assert_eq!(s.ball(104).unwrap().mode, space::MODE_STOP);
}
