use super::*;
use crate::space::{Ball, WarpState, MODE_STOP};

fn ball(id: i64, type_id: i32, x: f64) -> Ball {
    Ball {
        id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        interactive: false, massive: true, warp: WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE; run serially"]
async fn arcs_use_primary_range_and_stable_unique_distance_order() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let mut system = System::new(1, Arc::default(), 0);
    for b in [ball(1, ship, 100.0), ball(2, ship, 0.0), ball(5, ship, 5.0), ball(3, ship, -5.0), ball(4, ship, 10.0), ball(6, ship, 10.1)] { system.insert(b); }
    assert_eq!(select_targets(&system, &sde, 1, 2, 10.0, 3).unwrap(), vec![(3, None), (5, None), (4, None)]);
    let selected = select_targets(&system, &sde, 1, 2, 10.0, secondary_count(3.0)).unwrap();
    assert_eq!(selected.len() + 1, 3);
    assert!(selected.iter().all(|(id, _)| *id != 1 && *id != 2));
    assert_ne!(selected[0].0, selected[1].0);
    assert_eq!(select_targets(&system, &sde, 1, 2, 10.0, 1).unwrap(), vec![(3, None)]);
    assert!(select_targets(&system, &sde, 1, 2, f64::NAN, 3).unwrap().is_empty());
    assert!(select_targets(&system, &sde, 1, 99, 10.0, 3).is_none());
}

#[test]
fn arc_limit_includes_primary_and_handles_invalid_values() {
    assert_eq!(secondary_count(10.0) + 1, 10);
    assert_eq!(secondary_count(1.0), 0);
    assert_eq!(secondary_count(0.0), 0);
    assert_eq!(secondary_count(-1.0), 0);
    assert_eq!(secondary_count(f64::NAN), 0);
    assert_eq!(secondary_count(f64::INFINITY), 0);
    assert_eq!(secondary_count(2.9), 1);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE; run serially"]
async fn chain_weapon_types_have_both_arc_attributes() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    let weapons: Vec<_> = sde.types.iter().filter(|t| t.effects().iter()
        .any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_CHAIN_LIGHTNING)).collect();
    assert!(!weapons.is_empty());
    for weapon in weapons {
        assert!(weapon.attribute(ATTR_VORTON_ARC_RANGE).is_some_and(|v| v.is_finite() && v > 0.0));
        assert!(weapon.attribute(ATTR_VORTON_ARC_TARGETS).is_some_and(|v| v.is_finite() && v > 0.0));
    }
}
