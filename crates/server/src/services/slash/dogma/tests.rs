use super::*;

#[test]
fn drone_queries_select_only_the_session_owners_drone() {
    let mut item = Item {
        item_id: 42, type_id: 1, owner_id: 7, location_id: 1, flag_id: 1,
        quantity: 1, singleton: true, group_id: 1,
        category_id: i64::from(CATEGORY_DRONE), custom_info: None,
    };
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(7)), Some(Key::Drone(42)));
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(8)), None);
    assert_eq!(drone_query_key(&item, Action::Query("speed"), None), None);
    assert_eq!(drone_query_key(&item, Action::Set("speed", 2.0), Some(7)), None);
    assert_eq!(drone_query_key(&item, Action::Reset("speed"), Some(7)), None);
    assert_eq!(drone_query_key(&item, Action::List, Some(7)), None);
    item.category_id = i64::from(CATEGORY_MODULE);
    assert_eq!(drone_query_key(&item, Action::Query("speed"), Some(7)), None);
}

#[test]
fn live_drone_candidate_uses_item_location_and_rejects_bay_or_non_singletons() {
    let mut item = Item {
        item_id: 42, type_id: 1, owner_id: 7, location_id: 123, flag_id: 0,
        quantity: 1, singleton: true, group_id: 1,
        category_id: i64::from(CATEGORY_DRONE), custom_info: None,
    };
    assert_eq!(drone_location(&item), Some(123));
    item.location_id = 456;
    assert_eq!(drone_location(&item), Some(456));
    item.flag_id = 1;
    assert_eq!(drone_location(&item), None);
    item.flag_id = 0;
    item.singleton = false;
    assert_eq!(drone_location(&item), None);
    item.singleton = true;
    item.category_id = i64::from(CATEGORY_MODULE);
    assert_eq!(drone_location(&item), None);
}

#[test]
fn late_gm_snapshot_requests_refresh_without_overwriting_newer_projection() {
    crate::tests::init_config();
    use crate::space::{Ball, System, WarpState, MODE_STOP};
    use std::sync::Arc;

    let mut system = System::new(0, Arc::default(), 12_345);
    system.insert(Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: MODE_STOP, radius: 1.0,
        mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    });
    let old = system.projection_revision(1);
    system.refresh_projection_target(1);
    let newer = system.projection_revision(1);
    assert!(current_projection(&mut system, 1, newer));
    system.command(1, |ball, _, _| { ball.set_max_speed(10.0); Vec::new() });
    system.tick(13_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 10.0);
    if current_projection(&mut system, 1, old) {
        system.command(1, |ball, _, _| { ball.set_max_speed(100.0); Vec::new() });
    }
    assert!(system.projection_revision(1) > newer);
    system.tick(14_000);
    assert_eq!(system.ball(1).unwrap().max_vel, 10.0);
}
