//! Collision trajectory and wire-byte replay from frozen native numeric output.
//! Fixtures exercise existing System APIs without a native runtime or database.

use super::*;
use super::destiny_reference::{replay, replay_fractions, ship, STREAM_TICKS};

fn setup(name: &str) -> Vec<Ball> {
    match name {
        "station-sphere-deep-overlap" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                mass: 1e+34,
                radius: 33413.0,
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(60000004, [1723680890880.0, 256414064640.0, -60755435520.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                mass: 1480000.0,
                radius: 27.0,
                max_vel: 370.0,
                agility: 4.1,
                speed_fraction: 0.0,
                ..ship(1, [1723680887592.2163, 256414064640.0, -60755442260.955345], [-162.19732431195877, 0.0, -332.55379713069175])
            });
            balls
        }
        "dynamic-equal-head-on" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(1, [-50.0, 0.0, 0.0], [100.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(2, [50.0, 0.0, 0.0], [-100.0, 0.0, 0.0])
            });
            balls
        }
        "dynamic-unequal-head-on" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(1, [-50.0, 0.0, 0.0], [100.0, 0.0, 0.0])
            });
            balls.push(Ball {
                mass: 4000000.0,
                max_vel: 100.0,
                agility: 0.25,
                ..ship(2, [50.0, 0.0, 0.0], [-100.0, 0.0, 0.0])
            });
            balls
        }
        "stationary-contact-boundaries" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                ..ship(1, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                ..ship(2, [20.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                ..ship(3, [0.0, 100000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(4, [15.0, 100000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                ..ship(5, [0.0, 200000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                massive: false,
                interactive: false,
                ..ship(6, [15.0, 200000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                ..ship(7, [0.0, 300000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(8, [15.0, 300000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                cloak: 1,
                ..ship(9, [0.0, 400000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(10, [15.0, 400000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                ..ship(11, [0.0, 500000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                speed_fraction: 0.0,
                cloak: 1,
                ..ship(12, [15.0, 500000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        "fixed-fast-crossing" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 400.0,
                ..ship(1, [-100.0, 0.0, 0.0], [400.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(2, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 400.0,
                ..ship(3, [-100.0, 100000.0, 0.0], [400.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(4, [0.0, 100020.1, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        "coincident-overlap-mass-ratio" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(1, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                mass: 30000000.0,
                max_vel: 100.0,
                ..ship(2, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        "multiple-fixed-obstacles" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(1, [-50.0, 0.0, 0.0], [100.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(2, [0.0, 12.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(3, [15.0, -12.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(4, [-50.0, 100000.0, 0.0], [100.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(5, [0.0, 100000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(6, [-8.0, 100016.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        "collision-filter-lanes" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(1, [-50.0, 0.0, 0.0], [100.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                massive: false,
                interactive: false,
                ..ship(2, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 100.0,
                ..ship(3, [-50.0, 100000.0, 0.0], [100.0, 0.0, 0.0])
            });
            balls.push(Ball {
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(4, [0.0, 100000.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        "warp-through-obstacle" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                ..ship(1, [0.0, 0.0, 0.0], [200.0, 0.0, 0.0])
            });
            balls.push(Ball {
                radius: 1000.0,
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                interactive: false,
                ..ship(2, [1000000.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        "missile-owner-target" => {
            let mut balls = Vec::new();
            balls.push(Ball {
                radius: 30.0,
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                ..ship(1, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                radius: 1.0,
                max_vel: 1500.0,
                massive: false,
                interactive: false,
                ..ship(2, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls.push(Ball {
                radius: 30.0,
                max_vel: 0.0,
                speed_fraction: 0.0,
                mode: MODE_RIGID,
                free: false,
                global: true,
                ..ship(3, [10000.0, 0.0, 0.0], [0.0, 0.0, 0.0])
            });
            balls
        }
        _ => panic!("unknown collision scenario {name}"),
    }
}

fn commands(system: &mut System, name: &str) {
    match name {
        "station-sphere-deep-overlap" => {
        }
        "dynamic-equal-head-on" => {
            system.command(1, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
            system.command(2, |b, _, _| { b.goto_direction([-1.0, 0.0, 0.0]); vec![] });
        }
        "dynamic-unequal-head-on" => {
            system.command(1, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
            system.command(2, |b, _, _| { b.goto_direction([-1.0, 0.0, 0.0]); vec![] });
        }
        "stationary-contact-boundaries" => {
            system.command(8, |b, _, _| { b.cloak = 1; b.massive = false; vec![] });
        }
        "fixed-fast-crossing" => {
            system.command(1, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
            system.command(3, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
        }
        "coincident-overlap-mass-ratio" => {
        }
        "multiple-fixed-obstacles" => {
            system.command(1, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
            system.command(4, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
        }
        "collision-filter-lanes" => {
            system.command(4, |b, _, _| { b.cloak = 1; b.massive = false; vec![] });
            system.command(1, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
            system.command(3, |b, _, _| { b.goto_direction([1.0, 0.0, 0.0]); vec![] });
        }
        "warp-through-obstacle" => {
            system.command(1, |b, _, _| { b.warp_to([149597870700.0, 0.0, 0.0], 20000.0, 3000); vec![] });
        }
        "missile-owner-target" => {
            let missile = system.ball(2).unwrap().clone();
            system.launch_missile(missile, 3, 1, 121_000, |_| {});
        }
        _ => panic!("unknown collision scenario {name}"),
    }
}

macro_rules! scenario_tests {
    ($trajectory:ident, $stream:ident, $name:literal, $fixture:literal) => {
        #[test] fn $trajectory() { replay($name, include_str!($fixture), setup($name), &STREAM_TICKS, false, |s| commands(s, $name)); }
        #[test] fn $stream() { replay($name, include_str!($fixture), setup($name), &STREAM_TICKS, true, |s| commands(s, $name)); }
    };
}
scenario_tests!(station_sphere_deep_overlap_trajectory, station_sphere_deep_overlap_stream, "station-sphere-deep-overlap", "fixtures/station-sphere-deep-overlap.jsonl");
scenario_tests!(dynamic_equal_head_on_trajectory, dynamic_equal_head_on_stream, "dynamic-equal-head-on", "fixtures/dynamic-equal-head-on.jsonl");
scenario_tests!(dynamic_unequal_head_on_trajectory, dynamic_unequal_head_on_stream, "dynamic-unequal-head-on", "fixtures/dynamic-unequal-head-on.jsonl");
scenario_tests!(stationary_contact_boundaries_trajectory, stationary_contact_boundaries_stream, "stationary-contact-boundaries", "fixtures/stationary-contact-boundaries.jsonl");
scenario_tests!(fixed_fast_crossing_trajectory, fixed_fast_crossing_stream, "fixed-fast-crossing", "fixtures/fixed-fast-crossing.jsonl");
scenario_tests!(coincident_overlap_mass_ratio_trajectory, coincident_overlap_mass_ratio_stream, "coincident-overlap-mass-ratio", "fixtures/coincident-overlap-mass-ratio.jsonl");
scenario_tests!(multiple_fixed_obstacles_trajectory, multiple_fixed_obstacles_stream, "multiple-fixed-obstacles", "fixtures/multiple-fixed-obstacles.jsonl");
scenario_tests!(collision_filter_lanes_trajectory, collision_filter_lanes_stream, "collision-filter-lanes", "fixtures/collision-filter-lanes.jsonl");
scenario_tests!(warp_through_obstacle_trajectory, warp_through_obstacle_stream, "warp-through-obstacle", "fixtures/warp-through-obstacle.jsonl");
scenario_tests!(missile_owner_target_trajectory, missile_owner_target_stream, "missile-owner-target", "fixtures/missile-owner-target.jsonl");

macro_rules! fraction_test {
    ($test:ident, $name:literal, $fixture:literal) => {
        #[test] fn $test() { replay_fractions($name, include_str!($fixture), setup($name), |s| commands(s, $name)); }
    };
}
fraction_test!(station_sphere_deep_overlap_fractions, "station-sphere-deep-overlap", "fixtures/station-sphere-deep-overlap-fractions.jsonl");
fraction_test!(dynamic_unequal_head_on_fractions, "dynamic-unequal-head-on", "fixtures/dynamic-unequal-head-on-fractions.jsonl");
fraction_test!(stationary_contact_boundaries_fractions, "stationary-contact-boundaries", "fixtures/stationary-contact-boundaries-fractions.jsonl");
fraction_test!(fixed_fast_crossing_fractions, "fixed-fast-crossing", "fixtures/fixed-fast-crossing-fractions.jsonl");
fraction_test!(multiple_fixed_obstacles_fractions, "multiple-fixed-obstacles", "fixtures/multiple-fixed-obstacles-fractions.jsonl");

#[test] fn coincident_overlap_mass_ratio_reversed_trajectory() {
    let name = "coincident-overlap-mass-ratio";
    let mut balls = setup(name);
    balls.reverse();
    replay(name, include_str!("fixtures/coincident-overlap-mass-ratio.jsonl"), balls, &STREAM_TICKS, false, |s| commands(s, name));
}

#[test] fn coincident_overlap_mass_ratio_reversed_stream() {
    let name = "coincident-overlap-mass-ratio";
    let mut balls = setup(name);
    balls.reverse();
    replay(name, include_str!("fixtures/coincident-overlap-mass-ratio.jsonl"), balls, &STREAM_TICKS, true, |s| commands(s, name));
}

#[test] fn multiple_fixed_obstacles_reversed_trajectory() {
    let name = "multiple-fixed-obstacles";
    let mut balls = setup(name);
    balls.reverse();
    replay(name, include_str!("fixtures/multiple-fixed-obstacles.jsonl"), balls, &STREAM_TICKS, false, |s| commands(s, name));
}

#[test] fn multiple_fixed_obstacles_reversed_stream() {
    let name = "multiple-fixed-obstacles";
    let mut balls = setup(name);
    balls.reverse();
    replay(name, include_str!("fixtures/multiple-fixed-obstacles.jsonl"), balls, &STREAM_TICKS, true, |s| commands(s, name));
}
