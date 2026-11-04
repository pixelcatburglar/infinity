use super::*;

fn pilot(id: i64, character: i64) -> Ball {
    crate::tests::init_config();
    Ball {
        id, type_id: 1, owner_id: character, corp_id: 1, alliance_id: -1,
        char_id: Some(character), signature_radius: Some(1.0), mode: MODE_STOP, radius: 1.0, mass: 1e6,
        max_vel: 100.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: true,
        massive: true, warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn only_indexed_matching_ship_changes_affiliation() {
    let mut system = System::new(1, Arc::default(), 1000);
    system.insert(pilot(10, 1));
    system.insert(pilot(20, 2));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    system.observers.insert(2, (20, tx));
    system.refresh_pilot_affiliation(1, 20, Some(5), Some(6));
    assert_eq!(system.pilot_ball(1).unwrap().corp_id, 1);
    assert!(rx.try_recv().is_err());
    system.refresh_pilot_affiliation(1, 10, Some(5), Some(6));
    let ball = system.pilot_ball(1).unwrap();
    assert_eq!((ball.corp_id, ball.alliance_id), (5, 6));
    assert_eq!(rx.try_recv().unwrap(), vec![event(system.current_time, "OnSlimItemChange", vec![Value::Int(10), ball.slim_item()])]);
    system.refresh_pilot_affiliation(1, 10, Some(5), Some(6));
    assert!(rx.try_recv().is_err());
    assert_eq!((system.pilot_ball(2).unwrap().corp_id, system.pilot_ball(2).unwrap().alliance_id), (1, -1));
}
