use super::*;
use crate::space::{WarpState, MODE_GOTO, MODE_STOP};

fn locator(category: &str, position: V3, direction: V3) -> DirectionalLocator {
    DirectionalLocator { category: category.into(), position, direction }
}

fn ball(max_vel: f32) -> Ball {
    Ball { id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: Some(1), mode: MODE_STOP, radius: 10.0, signature_radius: None,
        mass: 1e6, max_vel, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: true, global: false, massive: true, interactive: true, warp: WarpState::default(),
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

fn close(a: V3, b: V3) { for i in 0..3 { assert!((a[i] - b[i]).abs() < 1e-12, "{a:?} != {b:?}"); } }

#[test]
fn categories_and_selection() {
    for group in [GROUP_FREIGHTER, GROUP_INDUSTRIAL, GROUP_TRANSPORT_SHIP, GROUP_BLOCKADE_RUNNER, GROUP_JUMP_FREIGHTER, GROUP_BATTLESHIP, GROUP_ELITE_BATTLESHIP, GROUP_BLACK_OPS, GROUP_MARAUDERS, GROUP_INDUSTRIAL_COMMAND_SHIP] { assert_eq!(category(Some(group)), "undockPointLarge"); }
    for group in [GROUP_CARRIER, GROUP_COMMAND_CARRIER, GROUP_FORCE_AUX, GROUP_DREADNOUGHT, GROUP_LANCER_DREADNOUGHT, GROUP_CAPITAL_INDUSTRIAL_SHIP] { assert_eq!(category(Some(group)), "undockPointCapital"); }
    for group in [GROUP_TITAN, GROUP_SUPERCARRIER] { assert_eq!(category(Some(group)), "undockPointSupercapital"); }
    assert_eq!(category(None), "undockPointSmall");
    assert_eq!(category(Some(25)), "undockPointSmall");
    let values = [locator("other", [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]), locator("undockPointLarge", [2.0, 0.0, 0.0], [0.0, 0.0, 1.0]), locator("undockPointLarge", [3.0, 0.0, 0.0], [0.0, 0.0, 1.0])];
    assert_eq!(select(&values, Some(GROUP_FREIGHTER), &mut || 0.75).unwrap().position[0], 3.0);
    assert_eq!(select(&values, None, &mut || 0.0).unwrap().position[0], 1.0);
    assert!(select(&[], None, &mut || 0.0).is_none());
}

#[test]
fn collision_projection_and_rounding() {
    let c = [1e16, -1e16, 1e16];
    let offset = [4.0, 0.0, 0.0];
    assert_eq!(position(c, offset, [1.0, 0.0, 0.0], 10.0, false), add(c, offset));
    for dir in [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], normalize([1.0, 1.0, 0.0])] {
        assert!(dist(position(c, offset, dir, 10.0, true), c) >= 10.0);
    }
    assert_eq!(position([0.0; 3], [10.0, 0.0, 0.0], [1.0, 0.0, 0.0], 10.0, true), [10.0, 0.0, 0.0]);
    assert_eq!(position([0.0; 3], [20.0, 0.0, 0.0], [-1.0, 0.0, 0.0], 10.0, true), [20.0, 0.0, 0.0]);
    assert_eq!(position([0.0; 3], [4.0, 0.0, 0.0], [1.0, 0.0, 0.0], 10.0, true), [10.0, 0.0, 0.0]);
    assert_eq!(position([0.0; 3], [4.0, 0.0, 0.0], [-1.0, 0.0, 0.0], 10.0, true), [-10.0, 0.0, 0.0]);
}

#[test]
fn d3dx_axis_and_composition() {
    let h = std::f64::consts::FRAC_PI_2;
    close(rotate([0.0, 0.0, 1.0], h, 0.0, 0.0), [1.0, 0.0, 0.0]);
    close(rotate([0.0, 0.0, 1.0], 0.0, h, 0.0), [0.0, -1.0, 0.0]);
    close(rotate([1.0, 0.0, 0.0], 0.0, 0.0, h), [0.0, 1.0, 0.0]);
    let v = [0.2, 0.4, 0.7];
    let (y, p, r): (f64, f64, f64) = (0.21, -0.17, 0.11);
    let (sr, cr) = r.sin_cos(); let (sp, cp) = p.sin_cos(); let (sy, cy) = y.sin_cos();
    let a = [cr*v[0]-sr*v[1], sr*v[0]+cr*v[1], v[2]];
    let b = [a[0], cp*a[1]-sp*a[2], sp*a[1]+cp*a[2]];
    close(rotate(v, y, p, r), [cy*b[0]+sy*b[2], b[1], -sy*b[0]+cy*b[2]]);
}

#[test]
fn preparation_sets_goto_speed_and_interpolation_anchors() {
    for (max, expected) in [(100.0, 175.0), (1000.0, 700.0), (0.0, 0.0)] {
        let mut b = ball(max);
        prepare(&mut b, &[], None, [1.0, 2.0, 3.0], 20.0, false, || 0.5);
        assert_eq!(b.mode, MODE_GOTO); assert_eq!(b.speed_fraction, 1.0);
        assert_eq!(b.pos, [1.0, 2.0, 3.0]); assert_eq!(b.old_pos, b.pos);
        assert_eq!(b.vel, [0.0, 0.0, expected]); assert_eq!(b.old_vel, b.vel);
        assert!(b.goto[2] > b.pos[2]);
    }
}

#[test]
fn jitter_uses_full_client_angle_bounds() {
    for random in [0.0, 1.0] {
        let mut b = ball(100.0);
        prepare(&mut b, &[], None, [0.0; 3], 0.0, false, || random);
        let angle = (random * 2.0 - 1.0) * JITTER;
        close(b.vel, scale(normalize(rotate([0.0, 0.0, 1.0], angle, angle, angle)), 175.0));
    }
}

#[test]
fn jitter_is_bounded_and_does_not_change_channel_placement() {
    let locators = [locator("undockPointSmall", [4.0, 0.0, 0.0], [1.0, 0.0, 0.0])];
    for yaw in [0.0, 0.5, 1.0] {
        for pitch in [0.0, 0.5, 1.0] {
            for roll in [0.0, 0.5, 1.0] {
                let mut values = [0.0, yaw, pitch, roll].into_iter();
                let mut b = ball(100.0);
                prepare(&mut b, &locators, None, [0.0; 3], 20.0, true, || values.next().unwrap());
                let clearance = 20.0 + f64::from(b.radius) + CLEARANCE;
                assert_eq!(b.pos, [clearance, 0.0, 0.0]);
                let angle = (b.vel[0] / 175.0).clamp(-1.0, 1.0).acos();
                assert!(angle <= 0.75 + 1e-12);
            }
        }
    }
}
