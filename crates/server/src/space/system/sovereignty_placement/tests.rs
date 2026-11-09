//! Database-free actor geometry capture regressions.
use super::*;
use crate::services::{combat::groups_health::{Condition, ShipStats}, fit::{Fit, Key}};
use crate::space::tests::destiny_reference::ship as test_ball;
use std::sync::Arc;

fn setup() -> (System, Arc<Sde>) {
    crate::tests::init_config();
    let groups = [GROUP_SUN, GROUP_SOVEREIGNTY_HUB, GROUP_CONTROL_TOWER, GROUP_STARGATE, i32::try_from(GROUP_STATION).unwrap()];
    let mut types: Vec<_> = groups.into_iter().enumerate().map(|(i, group)| (i as i32 + 2, Some(group))).collect();
    types.extend([(7, None), (8, Some(-1))]);
    let sde = crate::sde::types::tests::placement_fixture(&types);
    let mut system = System::new(1, Arc::default(), 1000);
    let mut pilot = test_ball(1, [1.0, 0.0, 0.0], [0.0; 3]);
    pilot.type_id = 1;
    pilot.char_id = Some(1);
    pilot.owner_id = 1;
    system.insert(pilot);
    let fit = Fit::for_space_type(sde.clone(), 1, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition_with_key(1, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit, Key::Ship);
    (system, sde)
}

#[test]
fn captures_only_relevant_groups_and_ignores_unknown_or_malformed_unrelated_balls() {
    let (mut system, sde) = setup();
    for type_id in 2..=8 {
        let mut ball = test_ball(i64::from(type_id), [f64::from(type_id), 0.0, 0.0], [0.0; 3]);
        ball.type_id = type_id;
        if type_id >= 7 { ball.radius = f32::NAN; ball.pos[0] = f64::NAN; }
        system.insert(ball);
    }
    let mut unknown = test_ball(9, [f64::NAN, 0.0, 0.0], [0.0; 3]);
    unknown.type_id = 999;
    unknown.radius = f32::NAN;
    system.insert(unknown);
    let proof = system.sovereignty_placement(1, &sde).unwrap();
    let mut groups: Vec<_> = proof.obstacles.iter().map(|o| (o.group_id, o.position[0] as i32)).collect();
    groups.sort_unstable();
    let mut expected = vec![(GROUP_SUN, 2), (GROUP_SOVEREIGNTY_HUB, 3), (GROUP_CONTROL_TOWER, 4), (GROUP_STARGATE, 5), (i32::try_from(GROUP_STATION).unwrap(), 6)];
    expected.sort_unstable();
    assert_eq!(groups, expected);
}

#[test]
fn relevant_obstacles_require_finite_position_and_valid_radius() {
    for group_type in 2..=6 {
        for (position, radius) in [([f64::NAN, 0.0, 0.0], 1.0), ([0.0; 3], f32::NAN), ([0.0; 3], -1.0)] {
            let (mut system, sde) = setup();
            let mut ball = test_ball(2, position, [0.0; 3]);
            ball.type_id = group_type;
            ball.radius = radius;
            system.insert(ball);
            assert!(system.sovereignty_placement(1, &sde).is_none());
        }
    }
}
