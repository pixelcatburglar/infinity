use super::*;

#[test]
fn propulsion_commands_wait_for_boundary_and_use_enqueue_stamp() {
    crate::tests::init_config();
    let mut system = System::new(0, Arc::default(), 12_345);
    let ball = Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    system.insert(ball);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    system.observe(1, 1, tx);
    queue_kinematics(&mut system, 1, Kinematics { speed: 20.0, mass: 30.0, agility: 0.25, signature: 40.0 });
    system.tick(12_999);
    assert_eq!(system.ball(1).unwrap().max_vel, 1.0);
    assert_eq!(system.ball(1).unwrap().agility, 1.0);
    assert!(rx.try_recv().is_err());
    system.tick(13_000);
    let ship = system.ball(1).unwrap();
    assert_eq!((ship.max_vel, ship.mass, ship.agility, ship.signature_radius), (20.0, 30.0, 0.25, Some(40.0)));
    let events = rx.try_recv().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0], event(12, "SetMaxSpeed", vec![Value::Int(1), Value::Float(20.0)]));
    assert_eq!(events[1], event(12, "SetBallMass", vec![Value::Int(1), Value::Float(30.0)]));
    assert_eq!(events[2], event(12, "SetBallAgility", vec![Value::Int(1), Value::Float(0.25)]));
    assert_eq!(system.current_time(), 13);
    use crate::services::effects::{projection::Projection, EffectKind, JammingType};
    let source = Ball { id: 2, ..system.ball(1).unwrap().clone() };
    system.insert(source);
    let old = system.projection_revision(1);
    system.queue_projection_kinematics(1, old, Kinematics { speed: 100.0, mass: 100.0, agility: 1.0, signature: 100.0 });
    system.upsert_projection(1, Projection {
        source_ship: 2, source_module: 3, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: i64::MAX,
        values: vec![], charge_values: vec![], charge_type: None,
    });
    let current = system.projection_revision(1);
    system.queue_projection_kinematics(1, current, Kinematics { speed: 10.0, mass: 30.0, agility: 0.25, signature: 40.0 });
    system.tick(14_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 10.0);
    system.queue_projection_kinematics(1, old, Kinematics { speed: 100.0, mass: 100.0, agility: 1.0, signature: 100.0 });
    system.tick(15_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 10.0);
    system.remove_projection(3);
    let restored = system.projection_revision(1);
    assert_ne!(restored, current);
    system.queue_projection_kinematics(1, current, Kinematics { speed: 10.0, mass: 30.0, agility: 0.25, signature: 40.0 });
    system.queue_projection_kinematics(1, restored, Kinematics { speed: 20.0, mass: 30.0, agility: 0.25, signature: 40.0 });
    system.tick(16_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 20.0);
}

#[tokio::test]
#[ignore = "requires a dedicated database with the retail SDE"]
async fn kinematics_reads_the_requested_fit_key() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut fit = Fit::for_space_type(sde, 587, vec![]);
    fit.include_drone(7, 2454, vec![(ATTR_MAX_VELOCITY, 123.0), (ATTR_MASS, 456.0),
        (crate::consts::dogma::ATTR_AGILITY, 0.75), (ATTR_SIGNATURE_RADIUS, 9.0)]);
    let drone = kinematics(&fit, Key::Drone(7));
    assert_eq!((drone.speed, drone.mass, drone.agility, drone.signature), (123.0, 456.0, 0.75, 9.0));
    assert_ne!(kinematics(&fit, Key::Ship).speed, drone.speed);
}
