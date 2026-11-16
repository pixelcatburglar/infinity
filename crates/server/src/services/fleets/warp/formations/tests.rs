use super::*;

fn cmd(pos: V3) -> MemberSnapshot { MemberSnapshot { pilot: 1, ship: 101, pos, radius: 0.0 } }
fn member(pilot: i64, pos: V3) -> MemberSnapshot { MemberSnapshot { pilot, ship: 100 + pilot, pos, radius: 0.0 } }
fn settings(kind: FormationKind) -> FormationSettings { FormationSettings { kind, spacing_m: 1_000.0, size_m: 10_000.0 } }
fn ends(v: &[PlannedPilot]) -> Vec<V3> { v.iter().map(|p| p.endpoint).collect() }
fn assert_v3(a: V3, b: V3) { for i in 0..3 { assert!((a[i] - b[i]).abs() < 1e-6, "{a:?} != {b:?}"); } }

#[test]
fn point_and_sphere_fill_bounded_volumes() {
    let t = [10_000.0, 0.0, 0.0];
    for (kind, radius) in [(FormationKind::Point, POINT_RADIUS_M), (FormationKind::Sphere, 5_000.0)] {
        let members = (2..6).map(|p| member(p, [0.0; 3])).chain(std::iter::once(cmd([0.0; 3])));
        let out = plan(settings(kind), WarpDestination::Exact(t), cmd([0.0; 3]), members).unwrap();
        assert_eq!(out.len(), 5);
        for e in ends(&out) { assert!(space::dist(e, t) <= radius, "{e:?}"); }
    }
}

#[test]
fn ball_radius_is_cube_root_of_volume_draw() {
    assert_eq!(ball_radius(2_500.0, 0.125), 1_250.0);
    assert_eq!(ball_radius(2_500.0, 0.0), 0.0);
}

#[test]
fn plane_ring_order_stays_world_horizontal_and_overflows() {
    let t = [10_000.0, 0.0, 0.0];
    let commander = cmd([0.0; 3]);
    let members = (2..12).map(|p| member(p, [0.0; 3])).chain(std::iter::once(commander));
    // K = 1 at d=5000, so ring 2 is real overflow beyond nominal capacity 9.
    let grid = FormationSettings { kind: FormationKind::Plane, spacing_m: 5_000.0, size_m: 10_000.0 };
    let out = ends(&plan(grid, WarpDestination::Exact(t), commander, members).unwrap());
    for e in &out { assert_eq!(e[1], 0.0); }                          // strictly XZ-horizontal
    assert_v3(out[0], [15_000.0, 0.0, 0.0]);                          // ring slot (0,1) -> +h
    assert_v3(out[1], [15_000.0, 0.0, 5_000.0]);                      // slot (1,1)
    assert_v3(out[7], [15_000.0, 0.0, -5_000.0]);                     // slot (-1,1), ring end
    assert_v3(out[8], [20_000.0, 0.0, 0.0]);                          // ring 2 overflow (0,2)
    assert_v3(out[10], t);                                            // commander centre, last
}

#[test]
fn wall_faces_incoming_and_survives_vertical_destination() {
    let commander = cmd([0.0; 3]);
    let m = || (2..4).map(|p| member(p, [0.0; 3])).chain(std::iter::once(commander));
    let out = ends(&plan(settings(FormationKind::Wall), WarpDestination::Exact([10_000.0, 0.0, 0.0]), commander, m()).unwrap());
    assert_v3(out[0], [10_000.0, 1_000.0, 0.0]);                      // (0,1) -> +u
    assert_v3(out[1], [10_000.0, 1_000.0, 1_000.0]);                  // (1,1) -> +u +r
    let out = ends(&plan(settings(FormationKind::Wall), WarpDestination::Exact([0.0, 10_000.0, 0.0]), commander, m()).unwrap());
    assert_v3(out[0], [1_000.0, 10_000.0, 0.0]);                      // degenerate up -> +X axis
    assert_v3(out[1], [1_000.0, 10_000.0, -1_000.0]);                 // r = cross(+Y, +X) = -Z
}

#[test]
fn arrow_fills_rows_behind_tip_and_overflows() {
    let t = [10_000.0, 0.0, 0.0];
    let commander = cmd([0.0; 3]);
    let members = (2..7).map(|p| member(p, [0.0; 3])).chain(std::iter::once(commander));
    // K = 1 at d=5000, so row 2 is real overflow beyond nominal capacity 4.
    let grid = FormationSettings { kind: FormationKind::Arrow, spacing_m: 5_000.0, size_m: 10_000.0 };
    let out = ends(&plan(grid, WarpDestination::Exact(t), commander, members).unwrap());
    assert_v3(out[5], t);                                             // commander tip
    assert_v3(out[0], [5_000.0, 0.0, 0.0]);                           // row 1 centre
    assert_v3(out[1], [5_000.0, 0.0, -5_000.0]);                      // row 1 i=-1
    assert_v3(out[2], [5_000.0, 0.0, 5_000.0]);                       // row 1 i=+1
    assert_v3(out[3], [0.0, 0.0, 0.0]);                               // row 2 centre (overflow)
    assert_v3(out[4], [0.0, 0.0, -5_000.0]);                          // row 2 i=-1
}

#[test]
fn relative_keeps_offsets_only_within_range() {
    let commander = cmd([0.0; 3]);
    let members = [member(2, [200_000.0, 0.0, 0.0]), member(3, [200_001.0, 0.0, 0.0]),
        member(4, [-200_000.0, 0.0, 0.0]), commander];
    let out = plan(settings(FormationKind::Relative), WarpDestination::Exact([50_000.0, 0.0, 0.0]), commander, members.into_iter()).unwrap();
    assert_eq!(out.len(), 3);                                         // out-of-range member dropped
    assert_eq!(out[0].pilot, 2);
    assert_v3(out[0].endpoint, [250_000.0, 0.0, 0.0]);                // pure translation
    assert_v3(out[1].endpoint, [-150_000.0, 0.0, 0.0]);               // 200 km boundary included
    assert_v3(out[2].endpoint, [50_000.0, 0.0, 0.0]);                 // commander zero offset
}

#[test]
fn object_clearance_is_minimal_rigid_shift() {
    let commander = cmd([-100_000.0, 0.0, 0.0]);                      // f = +X, o = -X, A = -2500
    let dest = WarpDestination::Ordinary { point: [0.0; 3], object_radius: Some(0.0), min_range: 0.0 };
    let run = |m2: V3| {
        let members = [member(2, [-97_500.0, 0.0, 0.0]), member(3, m2)];   // endpoints [0] and m2+[2500,0,0]
        ends(&plan(settings(FormationKind::Relative), dest, commander, members.into_iter()).unwrap())
    };
    assert_v3(run([-90_000.0, 0.0, 0.0])[0], [-2_500.0, 0.0, 0.0]);   // gap: far interval ignored
    assert_v3(run([-90_000.0, 0.0, 0.0])[1], [5_000.0, 0.0, 0.0]);
    assert_v3(run([-92_500.0, 0.0, 0.0])[1], [2_500.0, 0.0, 0.0]);    // touch: tangent stays
    let out = run([-94_500.0, 0.0, 1_500.0]);                         // overlap: chains to hi=5000
    assert_v3(out[0], [-5_000.0, 0.0, 0.0]);
    assert_v3(out[1], [-2_000.0, 0.0, 1_500.0]);
}

#[test]
fn ordinary_anchor_uses_standoff_and_nonfinite_rejects() {
    let commander = cmd([0.0; 3]);
    let dest = WarpDestination::Ordinary { point: [10_000.0, 0.0, 0.0], object_radius: None, min_range: 5_000.0 };
    let out = plan(settings(FormationKind::Plane), dest, commander, std::iter::once(commander)).unwrap();
    assert_v3(out[0].endpoint, [5_000.0, 0.0, 0.0]);                  // A = T - f * 5000
    let bad = member(2, [f64::NAN, 0.0, 0.0]);
    assert!(plan(settings(FormationKind::Plane), WarpDestination::Exact([0.0; 3]), commander, std::iter::once(bad)).is_none());
}
