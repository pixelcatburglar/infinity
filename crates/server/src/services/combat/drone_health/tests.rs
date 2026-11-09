use super::*;

#[test]
fn refresh_preserves_absolute_damage_and_updates_resonance() {
    let mut condition = Condition { now: [80.0, 100.0, 150.0], stats: ShipStats {
        max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 1e30,
    }, shield_at: filetime_now() + FILETIME_SECOND };
    let stats = ShipStats { max: [120.0, 400.0, 100.0], resonance: [[0.5; 4]; 3], shield_tau_ms: 0.0 };
    assert!(crate::space::combat::rebase_condition(&mut condition, stats.clone()));
    assert_eq!(condition.now, [80.0, 300.0, -50.0]);
    assert_eq!(condition.stats.resonance, stats.resonance);
}

#[test]
fn saved_damage_restores_against_effective_maxima() {
    let stats = ShipStats { max: [100.0, 200.0, 300.0], resonance: [[1.0; 4]; 3], shield_tau_ms: 0.0 };
    let (now, fractions) = restored_condition(&stats, [Some(80.0), Some(30.0), Some(40.0)], 0, 0);
    assert_eq!(now, [80.0, 170.0, 260.0]);
    assert_eq!(ConditionSeed(Condition { now, stats, shield_at: 0 }).fractions(), fractions);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn indexed_drone_repair_uses_actor_health() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let type_id = sde.types.iter().find(|t| {
        if t.category_id != Some(infinity_core::consts::CATEGORY_DRONE) { return false; }
        let fit = Fit::for_space_type(sde.clone(), t.type_id, Vec::new());
        ShipStats::from_fit(&fit, Key::Ship).max.iter().all(|value| value.is_finite() && *value > 0.0)
    }).unwrap().type_id;
    let id = i64::MIN + 7311;
    let ball = Ball {
        id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None,
        signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 0.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false, massive: true,
        interactive: false, warp: crate::space::WarpState::default(), last_ms: 0,
        interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0, cloak_until: 0,
        health: [0.5; 3], launcher: 0, slim: Vec::new(),
    };
    let spec = crate::space::drone::DroneSpec { type_id, bandwidth: 0.0, scan_resolution: 0.0,
        control_range: 0.0, max_target_range: 0.0, optimal_range: 0.0, orbit_range: 0.0,
        duration_ms: 0.0, effect_id: None };
    let mut system = crate::space::System::default();
    let fit = Fit::for_space_type(sde.clone(), type_id, Vec::new());
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let initial = stats.max.map(|v| v * 0.5);
    let condition = prepare_condition_with_fit_key(&db, id, &fit, Key::Ship).await.unwrap();
    let prepared = crate::space::drone::PreparedDrone { ball, spec, condition, fit, key: Key::Ship };
    system.restore_drone(prepared, Arc::new(|_, _| Some(Box::pin(async {}))));
    assert!(system.set_condition_fraction(id, 0.5));
    assert!(damage::repair_target_in_system(&mut system, id, 1, 25.0, crate::space::combat::RepairSource::Local).unwrap());
    let snapshot = system.combat_condition_snapshot(id).unwrap();
    assert_eq!(snapshot.condition.now[1], (initial[1] + 25.0).min(snapshot.condition.stats.max[1]));
    assert_eq!(snapshot.condition.now[2], initial[2]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn condition_save_and_guarded_death_do_not_resurrect_items() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner = 101;
    let system = 103;
    let id = crate::dao::items::creation::positioned(&tx, 1, owner, system, [1.0; 3]).await.unwrap();
    let ids = [ATTR_SHIELD_CHARGE, ATTR_ARMOR_DAMAGE, ATTR_HULL_DAMAGE];
    let batch = [(id, [80.0, 30.0, 40.0], filetime_now())];
    assert_eq!(crate::dao::items::dogma::save_conditions(&tx, system, &ids, &batch).await.unwrap(), vec![id]);
    let rows = crate::dao::items::dogma::overrides(&tx, id).await.unwrap();
    let value = |attribute| rows.iter().find(|r| r.attribute_id == attribute).map(|r| r.value);
    assert_eq!([value(ATTR_SHIELD_CHARGE), value(ATTR_ARMOR_DAMAGE), value(ATTR_HULL_DAMAGE)], [Some(80.0), Some(30.0), Some(40.0)]);
    assert!(crate::dao::items::drones::destroy(&tx, id, owner, system).await.unwrap());
    assert!(!crate::dao::items::drones::return_to_bay(&tx, id, owner, system, owner, 102).await.unwrap());
    assert!(crate::dao::items::dogma::save_conditions(&tx, system, &ids, &batch).await.unwrap().is_empty());
    assert!(crate::dao::items::dogma::overrides(&tx, id).await.unwrap().is_empty());
    let survivor = crate::dao::items::creation::positioned(&tx, 1, owner, system, [1.0; 3]).await.unwrap();
    assert!(crate::dao::items::drones::return_to_bay(&tx, survivor, owner, system, owner, 102).await.unwrap());
    assert!(!crate::dao::items::drones::destroy(&tx, survivor, owner, system).await.unwrap());
    tx.rollback().await.unwrap();
}
