use super::*;
use crate::space::{MODE_STOP, WarpState};

fn hull(id: i64) -> Ball {
    Ball {
        id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: 0, char_id: None,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: true, global: false,
        massive: true, interactive: false, warp: WarpState::default(), 
        last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[test]
fn free_hull_membership_is_idempotent_and_replacement_changes_generation() {
    crate::tests::init_config();
    let mut system = System::default();
    system.add_persistent_ship(hull(1));
    let first = system.persistent_generation(1).unwrap();
    assert_eq!(system.persistent_balls().count(), 1);
    assert_eq!(system.pilots().count(), 0);
    assert!(!system.ball(1).unwrap().interactive);
    assert!(system.snapshot_due > 0);
    system.add_persistent_ship(hull(1));
    assert_eq!(system.persistent_generation(1), Some(first));
    assert_eq!(system.persistent_balls().count(), 1);
    system.take(1);
    system.add_persistent_ship(hull(1));
    assert_ne!(system.persistent_generation(1), Some(first));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn restore_initializes_stance_once_and_keeps_the_deadline() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let stances = &crate::sde::ship_stances::HULL_STANCES[0];
    let system = sde.map.systems().next().unwrap().solar_system_id;
    let ship = crate::dao::items::creation::positioned(&db, stances.hull_type, fixture.character, system, [1.0, 2.0, 3.0]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let restored = world.restore_persistent_ships(system).await.unwrap();
    let ball = restored.iter().find(|b| b.ball.id == ship).map(|b| &b.ball).expect("restored hull");
    assert_eq!(ball.slim.iter().find(|(k, _)| *k == "shipStance").map(|(_, v)| v.clone()),
        Some(crate::services::ship_stance::state::stance_slim_value(None, 0, 1)));
    assert_eq!(db.execute("UPDATE game.ship_stances SET ready_at=222 WHERE ship_id=$1", &[&ship]).await.unwrap(), 1);
    world.restore_persistent_ships(system).await.unwrap();
    assert_eq!(crate::dao::items::ship_stance::timing(&db, ship).await.unwrap().unwrap().ready_at, 222);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
