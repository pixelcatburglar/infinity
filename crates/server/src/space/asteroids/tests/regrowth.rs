//! Durable regrowth publication and actor timer lifetime regressions.
//! These exercise the server's asteroid rules and database, not a client protocol.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn deferred_regrowth_reuses_rows_and_fences_replacement_then_cold_loads() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    assert!(regrowth() > 0);
    // A beltless known-space system avoids creating unrelated SDE belts during cold loads.
    let mut selected = None;
    for row in sde.map.systems() {
        let system = row.solar_system_id;
        if system >= infinity_core::consts::WORMHOLE_SYSTEM_START || cold_inputs(&sde, system).unwrap().len() != 0 { continue; }
        if !dao::load(&db, system).await.unwrap().is_empty() || !dao::fields(&db, system, &[]).await.unwrap().is_empty() { continue; }
        let (security, jspace) = band(&sde, system).unwrap();
        if generation_inputs(&sde, [0.0; 3], Field::Ore, security, jspace).rocks.is_empty() { continue; }
        selected = Some(system);
        break;
    }
    let system = selected.expect("dedicated SDE database needs an unused beltless known-space system with ore");
    let mut belt = None;
    for row in sde.map.systems() {
        for celestial in sde.map.celestials(row.solar_system_id) {
            if let Some(pos) = center(&sde, celestial.item_id) {
                if dao::field(&db, celestial.item_id).await.unwrap().is_none() {
                    belt = Some((celestial.item_id, pos));
                    break;
                }
            }
        }
        if belt.is_some() { break; }
    }
    let (deadline_field, deadline_pos) = belt.expect("dedicated SDE database needs an unused belt marker");
    let work_db = db.clone();
    let result = tokio::spawn(async move {
        let field: i64 = work_db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
        assert!(dao::grown_batch(&work_db, system, &[field], 1).await.unwrap());
        let id = dao::insert_batch(&work_db, system, &[dao::NewAsteroid {
            field, type_id: 1, quantity: 1, radius: 1.0, pos: [0.0; 3],
        }]).await.unwrap()[0].id;
        let world = World::new(work_db.clone(), sde.clone(), 1);
        let replacement_world = World::new(work_db.clone(), sde.clone(), 1);
        world.system(system).await.unwrap();
        replacement_world.system(system).await.unwrap();
        let old = world.asteroid_actor(system).unwrap();
        regrow(&work_db, &world, system, field, [0.0; 3], old.0.clone()).await.unwrap();
        let rows = dao::load(&work_db, system).await.unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![id]);
        assert_eq!(world.with(system, move |s| s.rock_field(id)).await, Some(Some(field)));

        let growth_field: i64 = work_db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
        let replacement = replacement_world.running(system).unwrap();
        let old_handle = world.systems.lock().unwrap().insert(system, replacement).unwrap();
        regrow(&work_db, &world, system, growth_field, [0.0; 3], old.0.clone()).await.unwrap();
        let grown: Vec<_> = dao::load(&work_db, system).await.unwrap().into_iter()
            .filter(|r| r.field == growth_field).map(|r| r.id).collect();
        assert!(!grown.is_empty());
        for &rock in &grown {
            assert_eq!(world.with(system, move |s| s.rock_field(rock)).await, Some(None));
        }
        assert!(dao::grown_batch(&work_db, system, &[deadline_field], 1).await.unwrap());
        let zero = dao::insert_batch(&work_db, system, &[dao::NewAsteroid {
            field: deadline_field, type_id: 1, quantity: 0, radius: 1.0, pos: deadline_pos,
        }]).await.unwrap()[0].id;
        drop(old_handle);
        drop(world);
        drop(replacement_world);
        let cold = World::new(work_db.clone(), sde, 1);
        cold.system(system).await.unwrap();
        for &rock in &grown {
            assert_eq!(cold.with(system, move |s| (s.rock_field(rock), s.ball(rock).is_some())).await,
                Some((Some(growth_field), true)));
        }
        assert_eq!(cold.with(system, move |s| s.rock_field(id)).await, Some(Some(field)));
        assert!(!dao::load(&work_db, system).await.unwrap().iter().any(|r| r.id == zero));
        let depleted = dao::field(&work_db, deadline_field).await.unwrap().unwrap().depleted_at.unwrap();
        let deadline = depleted.saturating_add(regrowth());
        assert_eq!(cold.with(system, move |s| s.asteroid_timers.get(&deadline_field).map(|v| v.0)).await, Some(Some(deadline)));
        let persisted: Vec<_> = dao::load(&work_db, system).await.unwrap().into_iter()
            .filter(|r| r.field == growth_field).map(|r| r.id).collect();
        assert_eq!(persisted, grown);
    }).await;
    db.execute("DELETE FROM game.asteroids WHERE system_id = $1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id = $1", &[&system]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn scheduled_asteroid_timer_does_not_retain_world() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system: i64 = db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let world = World::new(db, sde, 1);
    world.system(system).await.unwrap();
    let handle = world.running(system).unwrap();
    let weak = Arc::downgrade(&world);
    let at = filetime_now().saturating_add(regrowth().max(FILETIME_SECOND));
    handle.with(move |s| s.asteroid_timer(system, at, [0.0; 3])).await.unwrap();
    drop(world);
    assert!(weak.upgrade().is_none());
    let (done, receipt) = tokio::sync::oneshot::channel();
    handle.with(move |s| {
        s.tick(unix_ms_from_filetime_ceil(at));
        assert!(!s.asteroid_timers.contains_key(&system));
        s.defer(async move { let _ = done.send(()); });
    }).await.unwrap();
    receipt.await.unwrap();
    assert!(weak.upgrade().is_none());
}
