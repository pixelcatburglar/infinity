//! Native movement trajectory and Thunker byte replay through the queued System APIs.
//! Fixtures are frozen carbonengine output; no native runtime or database is needed.

use super::*;
use super::destiny_reference::{replay, replay_fractions, ship, STREAM_TICKS};

struct Scenario { name: &'static str, fixture: &'static str }


fn setup(name: &str) -> Vec<Ball> {
    let zero = [0.0; 3];
    match name {
        "follow-rigid-station" => {
            let mut station = ship(2, [10000.0, 0.0, 0.0], zero);
            station.mass = 1e20; station.radius = 1000.0; station.max_vel = 0.0;
            station.speed_fraction = 0.0; station.free = false; station.global = true; station.interactive = false;
            vec![ship(1, zero, zero), station]
        }
        "stop-full-speed" => vec![ship(1, zero, [200.0, 0.0, 0.0])],
        "orbit-static-and-moving" => {
            let mut station = ship(201, zero, zero);
            station.radius = 1000.0; station.max_vel = 0.0; station.speed_fraction = 0.0;
            station.free = false; station.global = true; station.interactive = false;
            let mut moving = ship(211, [0.0, 1000000.0, 0.0], [0.0, 100.0, 0.0]);
            moving.radius = 1000.0; moving.max_vel = 100.0;
            vec![station, ship(202, [1510.0, 0.0, 0.0], zero), ship(203, [21010.0, 0.0, 0.0], zero), moving,
                ship(212, [1510.0, 1000000.0, 0.0], zero), ship(213, [21010.0, 1000000.0, 0.0], zero)]
        }
        "approach-moving-ship" => {
            let mut target = ship(301, [10000.0, 0.0, 0.0], [100.0, 0.0, 0.0]); target.max_vel = 100.0;
            vec![target, ship(302, zero, zero)]
        }
        "missileflight" => {
            let mut missile = ship(402, [0.0, 100.0, 0.0], zero);
            missile.radius = 1.0; missile.max_vel = 1500.0; missile.massive = false; missile.interactive = false;
            let mut target = ship(403, [10000.0, 0.0, 0.0], [100.0, 0.0, 0.0]); target.max_vel = 100.0;
            vec![ship(401, zero, zero), missile, target]
        }
        "warp-1au" | "warp-150km" | "warp-50au" => (0..4).map(|i| {
            ship(101 + i, [0.0, i as f64 * 10000.0, 0.0], if i >= 2 { [-200.0, 0.0, 0.0] } else { zero })
        }).collect(),
        _ => panic!("unknown scenario {name}"),
    }
}

fn follow(system: &mut System, id: i64, target: i64, range: f32, mode: u8) {
    system.command(id, move |b, _, others| {
        if let Some(target) = others.ball(target) {
            if mode == MODE_ORBIT { b.orbit(target, range); } else { b.follow_ball(target, range); }
        }
        vec![]
    });
}

fn direction(system: &mut System, id: i64, v: V3) {
    system.command(id, move |b, _, _| { b.goto_direction(v); vec![] });
}

fn commands(system: &mut System, name: &str) {
    match name {
        "follow-rigid-station" => follow(system, 1, 2, 1500.0, MODE_FOLLOW),
        "stop-full-speed" => system.command(1, |b, _, _| { b.stop(); vec![] }),
        "orbit-static-and-moving" => {
            follow(system, 202, 201, 500.0, MODE_ORBIT);
            follow(system, 203, 201, 20000.0, MODE_ORBIT);
            direction(system, 211, [0.0, 1.0, 0.0]);
            follow(system, 212, 211, 500.0, MODE_ORBIT);
            follow(system, 213, 211, 20000.0, MODE_ORBIT);
        }
        "approach-moving-ship" => { direction(system, 301, [1.0, 0.0, 0.0]); follow(system, 302, 301, 500.0, MODE_FOLLOW); }
        "missileflight" => {
            direction(system, 403, [1.0, 0.0, 0.0]);
            let missile = system.ball(402).unwrap().clone();
            system.launch_missile(missile, 403, 401, 121_000, |_| {});
        }
        "warp-1au" | "warp-150km" | "warp-50au" => {
            let x = match name { "warp-1au" => 149597870700.0, "warp-150km" => 150000.0, _ => 7479893535000.0 };
            for i in 0..4 {
                let point = [x, i as f64 * 10000.0, 0.0];
                let factor = if i % 2 == 0 { 3000 } else { 13000 };
                system.command(101 + i, move |b, _, _| {
                    b.warp_to(point, 20000.0, factor);
                    vec![]
                });
                system.command(101 + i, |b, _, _| { b.set_speed_fraction(1.0); vec![] });
            }
        }
        _ => unreachable!(),
    }
}

fn replay_scenario(s: &Scenario, streams: bool) {
    const DENSE_STREAM_TICKS: [i64; 14] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 30, 60, 120];
    let stream_ticks = if s.name == "follow-rigid-station" || s.name == "stop-full-speed" {
        &STREAM_TICKS[..]
    } else {
        &DENSE_STREAM_TICKS[..]
    };
    replay(s.name, s.fixture, setup(s.name), stream_ticks, streams, |system| commands(system, s.name));
}

macro_rules! scenario_tests {
    ($trajectory:ident, $stream:ident, $name:literal, $file:literal) => {
        #[test] fn $trajectory() { replay_scenario(&Scenario { name: $name, fixture: include_str!($file) }, false); }
        #[test] fn $stream() { replay_scenario(&Scenario { name: $name, fixture: include_str!($file) }, true); }
    };
}
scenario_tests!(follow_trajectory, follow_stream, "follow-rigid-station", "fixtures/follow-rigid-station.jsonl");
scenario_tests!(stop_trajectory, stop_stream, "stop-full-speed", "fixtures/stop-full-speed.jsonl");
scenario_tests!(orbit_trajectory, orbit_stream, "orbit-static-and-moving", "fixtures/orbit-static-and-moving.jsonl");
scenario_tests!(approach_trajectory, approach_stream, "approach-moving-ship", "fixtures/approach-moving-ship.jsonl");
scenario_tests!(missile_trajectory, missile_stream, "missileflight", "fixtures/missileflight.jsonl");
scenario_tests!(warp_1au_trajectory, warp_1au_stream, "warp-1au", "fixtures/warp-1au.jsonl");
scenario_tests!(warp_150km_trajectory, warp_150km_stream, "warp-150km", "fixtures/warp-150km.jsonl");
scenario_tests!(warp_50au_trajectory, warp_50au_stream, "warp-50au", "fixtures/warp-50au.jsonl");

#[test] fn warp_50au_fractions() {
    let name = "warp-50au";
    replay_fractions(name, include_str!("fixtures/warp-50au-fractions.jsonl"), setup(name), |system| commands(system, name));
}
