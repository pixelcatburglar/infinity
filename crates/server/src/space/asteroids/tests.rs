//! Asteroid projection and transaction regressions.
use super::*;
use super::generation::Candidate;
mod cold_cancellation;
mod regrowth;

fn candidates() -> GenerationInputs {
    GenerationInputs { center: [0.0; 3], rocks: vec![
        Candidate { type_id: 1, quantity: 10, radius: 1.0, pos: [2.0, 0.0, 0.0] },
        Candidate { type_id: 1, quantity: 20, radius: 1.0, pos: [3.0, 0.0, 0.0] },
    ] }
}

#[test]
fn actor_projection_requires_matching_visible_ball_and_never_increases() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 1_000);
    system.insert(ball(1, 1, [0.0; 3], 1.0));
    system.install_cold_asteroids(Prepared { entries: vec![Entry {
        ball: ball(2, 7, [3.0, 0.0, 0.0], 1.0), rock: Rock { field: 8, type_id: 7, quantity: 10 },
    }], deadlines: vec![] });
    assert_eq!(system.rocks_in_range(1, 3.0), Some(vec![(2, 7, 10)]));
    assert_eq!(system.rocks_in_range(1, 2.0), Some(vec![]));
    assert_eq!(system.rocks_in_range(99, 10.0), None);
    system.apply_mining_delivery(2, 4);
    system.apply_mining_delivery(2, 9);
    assert_eq!(system.rocks_in_range(1, 3.0), Some(vec![(2, 7, 4)]));
    system.apply_mining_delivery(2, 0);
    assert_eq!(system.rock_type(2), None);
    assert_eq!(system.rock_field(2), Some(8));
    system.rocks.remove(&2);
    system.apply_mining_delivery(2, 10);
    assert_eq!(system.rock_type(2), None);
    assert_eq!(system.rock_field(2), None);
    system.rocks.insert(3, Rock { field: 8, type_id: 7, quantity: 1 });
    assert_eq!(system.rock_type(3), None);
    system.insert(ball(3, 6, [0.0; 3], 1.0));
    assert_eq!(system.rock_type(3), None);
}

#[test]
fn asteroid_timer_deduplicates_replacements_and_rounds_up() {
    crate::tests::init_config();
    let mut system = System::new(1, Arc::default(), 0);
    let at = crate::client::filetime_from_unix_ms(1_000) + 1;
    assert_eq!(unix_ms_from_filetime_ceil(at), 1_001);
    system.asteroid_timer(2, at, [0.0; 3]);
    let first = system.asteroid_timers[&2];
    system.asteroid_timer(2, at, [0.0; 3]);
    assert_eq!(system.asteroid_timers[&2], first);
    let later = crate::client::filetime_from_unix_ms(2_000);
    system.asteroid_timer(2, later, [0.0; 3]);
    let replacement = system.asteroid_timers[&2];
    assert_ne!(replacement.1, first.1);
    system.tick(1_000);
    assert_eq!(system.asteroid_timers[&2], replacement);
    system.tick(1_001);
    assert_eq!(system.asteroid_timers[&2], replacement);
    system.tick(2_000);
    assert!(!system.asteroid_timers.contains_key(&2));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn generation_failure_rolls_back_and_existing_fields_are_not_topped_up() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let system: i64 = tx.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let field = system;
    let sql = format!("CREATE FUNCTION pg_temp.reject_second_asteroid() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.field_id = {field} AND EXISTS (SELECT 1 FROM game.asteroids WHERE field_id = NEW.field_id) THEN RAISE EXCEPTION 'fixture insert failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER asteroid_fixture_reject BEFORE INSERT ON game.asteroids FOR EACH ROW EXECUTE FUNCTION pg_temp.reject_second_asteroid()");
    tx.batch_execute(&sql).await.unwrap();
    let save = tx.savepoint("failed_generation").await.unwrap();
    assert!(prepare_field(&save, system, field, 10, &candidates(), false).await.is_err());
    save.rollback().await.unwrap();
    assert!(dao::field(&tx, field).await.unwrap().is_none());
    assert!(dao::load(&tx, system).await.unwrap().is_empty());
    tx.batch_execute("DROP TRIGGER asteroid_fixture_reject ON game.asteroids").await.unwrap();
    let ids = prepare_field(&tx, system, field, 10, &candidates(), false).await.unwrap().ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(tx.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&ids[0], &system, &field]).await.unwrap(), 1);
    assert_eq!(prepare_field(&tx, system, field, 11, &candidates(), false).await.unwrap().ids(), vec![ids[1]]);
    assert_eq!(tx.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&ids[1], &system, &field]).await.unwrap(), 1);
    assert!(prepare_field(&tx, system, field, 12, &candidates(), false).await.unwrap().ids().is_empty());
    assert!(prepare_field(&tx, system + 1, field, 12, &candidates(), false).await.is_err());
    assert_eq!(dao::field(&tx, field).await.unwrap().unwrap().system, system);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn cold_prepare_recovers_zero_row_and_restores_deadline() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..cfg.clone()
    }).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let system: i64 = tx.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let mut belt = None;
    for row in sde.map.systems() {
        for celestial in sde.map.celestials(row.solar_system_id) {
            if celestial.group_id != Some(GROUP_ASTEROID_BELT as i64) { continue; }
            if let Some(pos) = center(&sde, celestial.item_id) {
                if dao::field(&tx, celestial.item_id).await.unwrap().is_none() {
                    belt = Some((celestial.item_id, pos));
                    break;
                }
            }
        }
        if belt.is_some() { break; }
    }
    let (field, pos) = belt.expect("dedicated database needs an unused SDE belt");
    assert!(regrowth() > 0);
    assert!(dao::grown_batch(&tx, system, &[field], 1).await.unwrap());
    let id = dao::insert_batch(&tx, system, &[dao::NewAsteroid {
        field, type_id: 1, quantity: 0, radius: 1.0, pos,
    }]).await.unwrap()[0].id;
    let now = filetime_now();
    let batch = prepare(&tx, &sde, system, now, &[(field, pos, candidates())]).await.unwrap();
    assert!(!batch.ids().contains(&id));
    assert!(batch.ids().is_empty());
    assert!(dao::load(&tx, system).await.unwrap().is_empty());
    assert_eq!(dao::field(&tx, field).await.unwrap().unwrap().depleted_at, Some(now));
    assert!(batch.deadlines.iter().any(|d| d.field == field && d.at == now.saturating_add(regrowth())));
    let second = prepare(&tx, &sde, system, now, &[(field, pos, candidates())]).await.unwrap();
    assert!(second.ids().is_empty());
    assert!(second.deadlines.iter().any(|d| d.field == field && d.at == now.saturating_add(regrowth())));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database"]
async fn cancelled_precommit_transaction_rolls_back_before_retry() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let system: i64 = db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let field = system;
    let work_db = db.clone();
    let (ready, mut receipt) = tokio::sync::mpsc::unbounded_channel();
    let barrier = Arc::new(tokio::sync::Notify::new());
    let task = tokio::spawn(async move {
        let inputs = Arc::new(candidates());
        work_db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
            |tx, _| {
                let ready = ready.clone();
                let barrier = barrier.clone();
                let inputs = inputs.clone();
                Box::pin(async move {
                    let prepared = prepare_field(tx, system, field, 1, &inputs, false).await?;
                    let _ = ready.send(prepared.ids());
                    barrier.notified().await;
                    Ok(prepared)
                })
            },
            |batch, _| async move { Ok(batch) },
        ).await
    });
    let aborted_ids = receipt.recv().await.expect("transaction reached pre-commit barrier");
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    let inputs = Arc::new(candidates());
    let retry = db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
        |tx, _| { let inputs = inputs.clone(); Box::pin(async move { prepare_field(tx, system, field, 1, &inputs, false).await }) },
        |batch, _| async move { Ok(batch) },
    ).await;
    let rows = dao::load(&db, system).await.unwrap();
    db.execute("DELETE FROM game.asteroids WHERE system_id = $1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id = $1", &[&system]).await.unwrap();
    let ids = retry.unwrap().ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(rows.len(), 2);
    assert!(ids.iter().all(|id| !aborted_ids.contains(id)));
    assert!(rows.iter().all(|row| ids.contains(&row.id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn worlds_reload_independently_and_fence_stale_publication() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..crate::config::tests::test_database()
    }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system: i64 = db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
    let field = system;
    let work_db = db.clone();
    let result = tokio::spawn(async move {
        assert!(dao::grown_batch(&work_db, system, &[field], 1).await.unwrap());
        let id = dao::insert_batch(&work_db, system, &[dao::NewAsteroid {
            field, type_id: 7, quantity: 10, radius: 1.0, pos: [3.0, 0.0, 0.0],
        }]).await.unwrap()[0].id;
        let first = World::new(work_db.clone(), sde.clone(), 1);
        let second = World::new(work_db.clone(), sde.clone(), 1);
        let (a, b) = tokio::join!(first.system(system), first.system(system));
        a.unwrap(); b.unwrap();
        work_db.execute("UPDATE game.asteroids SET type_id=$2, quantity=$3, original_quantity=$3 WHERE asteroid_id=$1", &[&id, &8_i32, &20_i64]).await.unwrap();
        second.system(system).await.unwrap();
        work_db.execute("UPDATE game.asteroids SET type_id=$2, quantity=$3, original_quantity=$3 WHERE asteroid_id=$1", &[&id, &7_i32, &10_i64]).await.unwrap();
        assert_eq!(first.with(system, move |s| (s.rock_type(id), s.rock_field(id), s.rocks[&id].quantity, s.ball(id).is_some())).await,
            Some((Some(7), Some(field), 10, true)));
        assert_eq!(second.with(system, move |s| (s.rock_type(id), s.rock_field(id), s.rocks[&id].quantity, s.ball(id).is_some())).await,
            Some((Some(8), Some(field), 20, true)));
        first.with(system, move |s| s.apply_mining_delivery(id, 4)).await.unwrap();
        assert_eq!(first.with(system, move |s| s.rocks[&id].quantity).await, Some(4));
        assert_eq!(second.with(system, move |s| (s.rock_type(id), s.rocks[&id].quantity)).await, Some((Some(8), 20)));
        let old = first.asteroid_actor(system).unwrap();
        assert_eq!(old.rock_field(id).await, Some(field));
        let deadline = filetime_now().saturating_add(regrowth().max(FILETIME_SECOND));
        for world in [&first, &second] {
            world.with(system, move |s| s.asteroid_timer(field, deadline, [0.0; 3])).await.unwrap();
        }
        assert!(publish_depletion(&first, system, &old, id, field, None, None).await);
        first.with(system, move |s| { s.asteroid_timers.remove(&field); }).await.unwrap();
        assert_eq!(first.with(system, move |s| (s.rock_field(id), s.ball(id).is_some(), s.asteroid_timers.contains_key(&field))).await,
            Some((None, false, false)));
        assert_eq!(second.with(system, move |s| (s.rock_type(id), s.rock_field(id), s.rocks[&id].quantity, s.ball(id).is_some(), s.asteroid_timers.contains_key(&field))).await,
            Some((Some(8), Some(field), 20, true, true)));
        first.systems.lock().unwrap().remove(&system);
        first.system(system).await.unwrap();
        assert_eq!(first.with(system, move |s| s.rocks[&id].quantity).await, Some(10));
        let stale_field: i64 = work_db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
        let inputs = Arc::new(candidates());
        let stale = work_db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
            |tx, _| { let inputs = inputs.clone(); Box::pin(async move { prepare_field(tx, system, stale_field, 1, &inputs, false).await }) },
            |batch, _| async move { Ok(batch) },
        ).await.unwrap();
        let stale_ids = stale.ids();
        assert!(!publish(&first, system, &old, stale).await);
        for id in stale_ids {
            assert_eq!(first.with(system, move |s| s.rock_type(id)).await, Some(None));
        }
        let live = first.asteroid_actor(system).unwrap();
        let live_field: i64 = work_db.query_one("SELECT nextval('game.asteroid_seq') AS id", &[]).await.unwrap().get("id");
        let batch = work_db.transaction::<_, _, CallError, crate::db::outbox::Outbox<()>, _, _, _>(
            |tx, _| { let inputs = inputs.clone(); Box::pin(async move { prepare_field(tx, system, live_field, 1, &inputs, false).await }) },
            |batch, _| async move { Ok(batch) },
        ).await.unwrap();
        let live_ids = batch.ids();
        let (entered, entered_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        first.post(system, move |_| {
            let _ = entered.send(());
            let _ = release_rx.recv();
        });
        tokio::task::spawn_blocking(move || entered_rx.recv()).await.unwrap().unwrap();
        {
            let publication = publish(&first, system, &live, batch);
            tokio::pin!(publication);
            tokio::select! {
                biased;
                _ = &mut publication => panic!("publication acknowledged before actor action"),
                _ = tokio::task::yield_now() => {}
            }
            release.send(()).unwrap();
            assert!(publication.await);
        }
        for &id in &live_ids {
            assert_eq!(first.with(system, move |s| (s.rock_type(id), s.ball(id).is_some())).await,
                Some((Some(1), true)));
        }
        let depleted_id = live_ids[0];
        first.with(system, move |s| s.apply_mining_delivery(depleted_id, 0)).await.unwrap();
        assert_eq!(live.rock_field(depleted_id).await, Some(live_field));
        let result = work_db.transaction::<_, _, crate::db::Error, crate::db::outbox::Outbox<()>, _, _, _>(
            |tx, _| Box::pin(async move {
                assert!(dao::take(tx, depleted_id, 10, 10).await?);
                dao::deplete_scoped(tx, system, live_field, depleted_id, 1, None).await
            }),
            |result, _| async move { Ok(result) },
        ).await.unwrap();
        assert_eq!(result, dao::Depletion::Deleted { field: live_field, deadline: None });
        assert!(publish_depletion(&first, system, &live, depleted_id, live_field, None, None).await);
        assert_eq!(first.with(system, move |s| (s.rock_field(depleted_id), s.ball(depleted_id).is_some())).await,
            Some((None, false)));
        drop(first); drop(second);
        let recreated = World::new(work_db.clone(), sde, 1);
        recreated.system(system).await.unwrap();
        assert_eq!(recreated.with(system, move |s| (s.rock_type(id), s.rocks[&id].quantity)).await,
            Some((Some(7), 10)));
    }).await;
    db.execute("DELETE FROM game.asteroids WHERE system_id = $1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id = $1", &[&system]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn cold_commit_survives_hub_collision_and_retry_reuses_partial_field() {
    crate::tests::init_config();
    let cfg = crate::config::tests::test_database();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig {
        schema_on_start: false, ..cfg.clone()
    }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let mut selected = None;
    for row in sde.map.systems() {
        let system = row.solar_system_id;
        let Ok(inputs) = cold_inputs(&sde, system) else { continue };
        if inputs.iter().filter(|(_, _, input)| !input.rocks.is_empty()).count() < 2 { continue; }
        let Some(celestial) = sde.map.celestials(system).find(|c| {
            c.type_id.and_then(|t| i32::try_from(t).ok()).is_some_and(|t| sde.types.get(t).is_some())
                && [c.x, c.y, c.z].iter().all(|v| v.is_some_and(f64::is_finite))
        }) else { continue };
        let collision = celestial.item_id;
        let clean: bool = db.query_one("SELECT NOT EXISTS (SELECT 1 FROM game.asteroids WHERE system_id=$1) AND NOT EXISTS (SELECT 1 FROM game.asteroid_fields WHERE system_id=$1) AND NOT EXISTS (SELECT 1 FROM game.sov_systems WHERE system_id=$1) AND NOT EXISTS (SELECT 1 FROM game.items WHERE item_id=$2) AS clean", &[&system, &collision]).await.unwrap().get("clean");
        if !clean { continue; }
        let (field, pos, _) = inputs.iter().find(|(_, _, input)| !input.rocks.is_empty()).unwrap();
        selected = Some((system, collision, [celestial.x.unwrap(), celestial.y.unwrap(), celestial.z.unwrap()], *field, *pos));
        break;
    }
    let (system, collision, position, field, field_pos) = selected.expect("dedicated database needs an unused SDE system with two generating belts");
    let row = db.query_one("SELECT nextval('game.corporations_corporation_id_seq') AS corp, nextval('game.alliances_alliance_id_seq') AS alliance", &[]).await.unwrap();
    let corp: i64 = row.get("corp");
    let alliance: i64 = row.get("alliance");
    let hub = loop {
        let candidate: i64 = db.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
        if sde.map.celestial(candidate).is_none()
            && db.query_opt("SELECT 1 FROM game.items WHERE item_id=$1", &[&candidate]).await.unwrap().is_none()
        {
            break candidate;
        }
    };
    let namespace = format!("asteroid-sov-{system}-{collision}");
    let work_db = db.clone();
    let ns = namespace.clone();
    let result = tokio::spawn(async move {
        let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let tx = client.transaction().await.unwrap();
        let empty: &[u8] = &[];
        let fingerprint: &[u8] = &[0; 32];
        let hub_type = sde.sovereignty.hub().type_id;
        let now = filetime_now();
        tx.execute("INSERT INTO game.lua_content_versions (namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ($1,'1',1,'test',$2,$2,$2,$3)", &[&ns, &empty, &fingerprint]).await.unwrap();
        tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,$3,$4,0,1,true,$5,$6,$7)", &[&collision, &hub_type, &corp, &system, &position[0], &position[1], &position[2]]).await.unwrap();
        tx.execute("INSERT INTO game.sov_systems (system_id,hub_item_id,claim_corporation_id,alliance_id,claim_time,ownership_epoch,revision) VALUES ($1,$2,$3,$4,1,1,7)", &[&system, &collision, &corp, &alliance]).await.unwrap();
        tx.execute("INSERT INTO game.sov_structures (item_id,system_id,source_item_id,deployer_character_id,deployer_corporation_id,deployed_at,content_namespace,content_version,self_destruct_due_at,self_destruct_actor) VALUES ($1,$2,$1,1,$3,1,$4,'1',NULL,NULL)", &[&collision, &system, &corp, &ns]).await.unwrap();
        tx.execute("INSERT INTO game.sov_capture_targets (target_item_id,system_id,ownership_epoch,controlling_team,scoring_team,defender_team,score,rate,blocked,score_sim_time,checkpoint_wall_time,revision,capture_time_seconds,defensive_regen_rate,checkpoint_interval_ms,content_namespace,content_version) VALUES ($1,$2,1,NULL,NULL,$3,0.0,0.0,false,$4,0,3,10.0,0.0,1000,$5,'1')", &[&collision, &system, &alliance, &now, &ns]).await.unwrap();
        tx.execute("INSERT INTO game.sov_alliance_policy (alliance_id,current_capital_system,last_capital_changed_at,capital_lost_at,capital_change_not_before,revision) VALUES ($1,$2,11,NULL,22,5)", &[&alliance, &system]).await.unwrap();
        assert!(dao::grown_batch(&tx, system, &[field], now).await.unwrap());
        let partial = dao::insert_batch(&tx, system, &[dao::NewAsteroid {
            field, type_id: 1, quantity: 1, radius: 1.0, pos: field_pos,
        }]).await.unwrap()[0].id;
        tx.commit().await.unwrap();
        let world = World::new(work_db.clone(), sde.clone(), 1);
        crate::space::sovereignty::open(&world);
        let error = world.system(system).await.unwrap_err();
        assert!(format!("{error:#}").contains("Hub item collides with existing ball"));
        assert!(world.asteroid_actor(system).is_none());
        assert_eq!(world.with(system, |_| ()).await, None);
        let before_fields = work_db.query("SELECT field_id,system_id,grown_at,depleted_at FROM game.asteroid_fields WHERE system_id=$1 ORDER BY field_id", &[&system]).await.unwrap().iter().map(|r| (r.get::<_, i64>("field_id"), r.get::<_, i64>("system_id"), r.get::<_, i64>("grown_at"), r.get::<_, Option<i64>>("depleted_at"))).collect::<Vec<_>>();
        let before_rocks = work_db.query("SELECT asteroid_id,field_id,type_id,quantity,original_quantity,radius,x,y,z FROM game.asteroids WHERE system_id=$1 ORDER BY asteroid_id", &[&system]).await.unwrap().iter().map(|r| (r.get::<_, i64>("asteroid_id"), r.get::<_, i64>("field_id"), r.get::<_, i32>("type_id"), r.get::<_, i64>("quantity"), r.get::<_, i64>("original_quantity"), r.get::<_, Option<f32>>("radius"), r.get::<_, f64>("x"), r.get::<_, f64>("y"), r.get::<_, f64>("z"))).collect::<Vec<_>>();
        assert!(!before_fields.is_empty() && before_rocks.len() > 1);
        assert_eq!(before_rocks.iter().filter(|r| r.1 == field).map(|r| r.0).collect::<Vec<_>>(), vec![partial]);
        let tx = client.transaction().await.unwrap();
        tx.execute("INSERT INTO game.items (item_id,type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,$3,$4,0,1,true,$5,$6,$7)", &[&hub, &hub_type, &corp, &system, &position[0], &position[1], &position[2]]).await.unwrap();
        tx.execute("DELETE FROM game.sov_capture_targets WHERE target_item_id=$1", &[&collision]).await.unwrap();
        tx.execute("DELETE FROM game.sov_structures WHERE item_id=$1", &[&collision]).await.unwrap();
        tx.execute("UPDATE game.sov_systems SET hub_item_id=$2 WHERE system_id=$1 AND hub_item_id=$3", &[&system, &hub, &collision]).await.unwrap();
        tx.execute("INSERT INTO game.sov_structures (item_id,system_id,source_item_id,deployer_character_id,deployer_corporation_id,deployed_at,content_namespace,content_version,self_destruct_due_at,self_destruct_actor) VALUES ($1,$2,$1,1,$3,1,$4,'1',NULL,NULL)", &[&hub, &system, &corp, &ns]).await.unwrap();
        tx.execute("INSERT INTO game.sov_capture_targets (target_item_id,system_id,ownership_epoch,controlling_team,scoring_team,defender_team,score,rate,blocked,score_sim_time,checkpoint_wall_time,revision,capture_time_seconds,defensive_regen_rate,checkpoint_interval_ms,content_namespace,content_version) VALUES ($1,$2,1,NULL,NULL,$3,0.0,0.0,false,$4,0,3,10.0,0.0,1000,$5,'1')", &[&hub, &system, &alliance, &now, &ns]).await.unwrap();
        tx.execute("DELETE FROM game.items WHERE item_id=$1", &[&collision]).await.unwrap();
        tx.commit().await.unwrap();
        world.system(system).await.unwrap();
        let after_fields = work_db.query("SELECT field_id,system_id,grown_at,depleted_at FROM game.asteroid_fields WHERE system_id=$1 ORDER BY field_id", &[&system]).await.unwrap().iter().map(|r| (r.get::<_, i64>("field_id"), r.get::<_, i64>("system_id"), r.get::<_, i64>("grown_at"), r.get::<_, Option<i64>>("depleted_at"))).collect::<Vec<_>>();
        let after_rocks = work_db.query("SELECT asteroid_id,field_id,type_id,quantity,original_quantity,radius,x,y,z FROM game.asteroids WHERE system_id=$1 ORDER BY asteroid_id", &[&system]).await.unwrap().iter().map(|r| (r.get::<_, i64>("asteroid_id"), r.get::<_, i64>("field_id"), r.get::<_, i32>("type_id"), r.get::<_, i64>("quantity"), r.get::<_, i64>("original_quantity"), r.get::<_, Option<f32>>("radius"), r.get::<_, f64>("x"), r.get::<_, f64>("y"), r.get::<_, f64>("z"))).collect::<Vec<_>>();
        assert_eq!(before_fields, after_fields);
        assert_eq!(before_rocks, after_rocks);
        let expected = before_rocks.iter().map(|r| (r.0, r.1)).collect::<Vec<_>>();
        world.with(system, move |s| {
            assert_eq!(s.rocks.len(), expected.len());
            for (id, field) in expected { assert_eq!(s.rock_field(id), Some(field)); assert!(s.ball(id).is_some()); }
            assert!(s.ball(hub).is_some());
            assert_eq!(s.sovereignty.hub, Some(hub));
            let policy = &s.sovereignty.policies[0];
            assert_eq!((policy.alliance_id, policy.current_capital_system, policy.last_capital_changed_at, policy.capital_lost_at, policy.capital_change_not_before, policy.revision), (alliance, Some(system), Some(11), None, Some(22), 5));
        }).await.unwrap();
    }).await;
    db.execute("DELETE FROM game.sov_capture_targets WHERE system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.sov_structures WHERE system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.sov_alliance_policy WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.sov_systems WHERE system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1 OR item_id=$2", &[&collision, &hub]).await.unwrap();
    db.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1", &[&namespace]).await.unwrap();
    db.execute("DELETE FROM game.asteroids WHERE system_id=$1", &[&system]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1", &[&system]).await.unwrap();
    result.unwrap();
}
