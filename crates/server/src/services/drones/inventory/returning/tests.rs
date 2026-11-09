use super::*;

#[test]
fn scoop_requires_unique_positive_integer_ids_and_sorts_locks() {
    assert!(matches!(parse(&[Value::list([Value::Int(8), Value::Int(3)])]), Ok(ids) if ids == vec![3, 8]));
    for values in [vec![], vec![Value::Int(0)], vec![Value::Int(-1)], vec![Value::Int(3), Value::Int(3)], vec![Value::Bool(true)]] {
        assert!(parse(&[Value::list(values)]).is_err());
    }
    assert!(parse(&[]).is_err());
    assert!(parse(&[Value::Int(3)]).is_err());
}


#[tokio::test]
#[ignore]
async fn concurrent_death_and_scoop_have_one_guarded_winner() {
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut first, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let (mut second, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let setup = first.transaction().await.unwrap();
    let id = crate::dao::items::creation::positioned(&setup, 1, 101, 103, [1.0, 2.0, 3.0]).await.unwrap();
    setup.commit().await.unwrap();
    let result = crate::tests::catch_scenario(async {
    let (scooped, dead) = tokio::join!(
        async {
            let tx = first.transaction().await.unwrap();
            let won = drones::return_to_bay(&tx, id, 101, 103, 102, 104).await.unwrap();
            tx.commit().await.unwrap();
            won
        },
        async {
            let tx = second.transaction().await.unwrap();
            let won = drones::destroy(&tx, id, 101, 103).await.unwrap();
            tx.commit().await.unwrap();
            won
        }
    );
    assert_ne!(scooped, dead);
    }).await;
    first.execute("DELETE FROM game.items WHERE item_id = $1", &[&id]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

use crate::services::drones::tests::fixture::Fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn lost_controller_cannot_commit_unclaimed_contact() {
    let f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
    let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
    let id = crate::dao::items::creation::positioned(&f.db, ty, f.owner, f.system, [0.0; 3]).await.unwrap();
    let entries = super::super::restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let bandwidth = entries[0].spec.bandwidth;
    let owner = f.owner; let ship = f.ship;
    let token = f.world.with(f.system, move |s| s.reserve_drone_launch(owner, ship, &[bandwidth], 100.0, 1_000_000.0)).await.unwrap().unwrap();
    let effects = super::super::super::worker::effects(f.db.clone(), Arc::downgrade(&f.world), f.system);
    let ack = f.world.with(f.system, move |s| s.complete_drone_launch(token, owner, ship, entries, effects)).await.unwrap().unwrap();
    ack.await.unwrap();
    let snapshot = f.world.with(f.system, move |s| s.drone(id).unwrap()).await.unwrap();
    f.world.with(f.system, move |s| s.drone_controller_lost(ship)).await.unwrap();
    contact(&f.db, &f.world, f.system, snapshot, true).await.ok().unwrap();
    let row = f.db.query_one("SELECT location_id, owner_id FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), f.system);
    assert_eq!(row.get::<_, i64>("owner_id"), owner);
    assert!(f.world.with(f.system, move |s| s.ball(id).is_some() && s.drone(id).is_some_and(|d| d.controller.is_none())).await.unwrap());
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn rejected_return_keeps_native_damage_and_checkpoint_pending() {
    let mut f = Fixture::new().await;
    let mut trigger = None;
    let mut function = None;
    let result = crate::tests::catch_scenario(async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).unwrap().type_id;
        f.train_drone(ty).await;
        let id = f.drone(ty, 1, true).await;
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        let launch = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
        f.call("ship", "LaunchDrones", &launch).await.ok().unwrap();
        let (system, ship, owner) = (f.system, f.ship, f.owner);
        let market = crate::services::market::lock().await;
        let gate = f.world.lock_condition_snapshot(system).await.unwrap();
        let fit = Fit::load_in_space(&f.db, &f.world, system, owner, ship).await.ok().unwrap();
        let (drone, transition, before) = f.world.with(system, move |s| {
            let generation = s.combat_membership_generation(ship).unwrap();
            let facts = s.prepare_combat_source(ship, None, None, None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
            let hit = s.apply_damage(id, [1.0, 0.0, 0.0, 0.0],
                crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false).unwrap().unwrap();
            assert!(hit.dealt > 0.0 && !hit.lethal);
            let before = s.combat_condition_snapshot(id).unwrap();
            (s.drone(id).unwrap(), s.begin_condition_transitions(&[id]).unwrap(), before)
        }).await.unwrap();
        let frame = transition.facts.clone().unwrap();
        let before_rows: Vec<_> = crate::dao::items::dogma::overrides(&f.db, id).await.unwrap().into_iter()
            .map(|row| (row.attribute_id, row.value, row.updated_at)).collect();
        let event = frame.events().next().unwrap();
        let key = crate::db::outbox::SourceKey::canonical(event.event.kind(), event.identity, event.ordinal);
        let name = format!("drone_return_fact_fault_{id}");
        let function_name = format!("game.{name}_fn");
        f.db.execute(&format!("CREATE FUNCTION {function_name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.source_key = '{}' THEN RAISE EXCEPTION 'drone return fixture failure'; END IF; RETURN NEW; END $$", key.as_str()), &[]).await.unwrap();
        function = Some(function_name.clone());
        f.db.execute(&format!("CREATE TRIGGER {name} BEFORE INSERT ON game.progress_events FOR EACH ROW EXECUTE FUNCTION {function_name}()"), &[]).await.unwrap();
        trigger = Some(name);
        assert!(contact_return_claimed(&f.db, &f.world, system, &drone, ship, fit, transition, market, gate).await.is_err());
        assert!(!frame.is_committed(&f.db).await.unwrap());
        let after_rows: Vec<_> = crate::dao::items::dogma::overrides(&f.db, id).await.unwrap().into_iter()
            .map(|row| (row.attribute_id, row.value, row.updated_at)).collect();
        assert_eq!(after_rows, before_rows);
        let item = load_items(&f.db, &f.sde, ItemQuery::Id(id)).await.unwrap().pop().unwrap();
        assert_eq!((item.location_id, item.owner_id, item.flag_id), (system, owner, 0));
        let after = f.world.with(system, move |s| s.combat_condition_snapshot(id).unwrap()).await.unwrap();
        assert_eq!(after.condition.now, before.condition.now);
        assert_eq!(after.revision, before.revision);
        assert_ne!(after.revision, after.saved_revision);
    }).await;
    if let Some(name) = trigger { f.db.execute(&format!("DROP TRIGGER {name} ON game.progress_events"), &[]).await.unwrap(); }
    if let Some(name) = function { f.db.execute(&format!("DROP FUNCTION {name}()"), &[]).await.unwrap(); }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn dropped_scoop_caller_still_checkpoints_and_moves_drone() {
    blocked_scoop(false).await;
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn stale_later_scoop_membership_does_not_strand_other_drone() {
    blocked_scoop(true).await;
}

async fn blocked_scoop(replace_later: bool) {
    let mut f = Fixture::new().await;
    let mut fixture_ids = Vec::new();
    let mut row_driver = None;
    let result = crate::tests::catch_scenario(async {
        tokio::time::timeout(std::time::Duration::from_secs(150), async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).unwrap().type_id;
        f.train_drone(ty).await;
        fixture_ids.push(f.drone(ty, 1, true).await);
        fixture_ids.push(f.drone(ty, 1, true).await);
        fixture_ids.sort_unstable();
        let ids = fixture_ids.clone();
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        let launch = [Value::list(ids.iter().map(|&id| Value::tuple([Value::Int(id), Value::Int(1)]))), Value::None, Value::Bool(false)];
        let expected = Value::Dict(ids.iter().map(|&id| (Value::Int(id), Value::list([Value::Int(id)]))).collect());
        assert_eq!(f.call("ship", "LaunchDrones", &launch).await.ok().unwrap(), expected);
        for &id in &ids { super::super::abandon(&mut f.client, id).await.ok().unwrap(); }
        let (system, ship) = (f.system, f.ship);
        let victim = ids[0];
        let frame = f.world.with(system, move |s| {
            let generation = s.combat_membership_generation(ship).unwrap();
            let facts = s.prepare_combat_source(ship, None, None, None,
                crate::lua::events::resources::FactOrigin::Natural, generation, 1, 1, |_| true).unwrap();
            let hit = s.apply_damage(victim, [1.0, 0.0, 0.0, 0.0],
                crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false).unwrap().unwrap();
            assert!(hit.dealt > 0.0 && !hit.lethal);
            crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()
        }).await.unwrap();
        assert!(!frame.is_committed(&f.db).await.unwrap());
        let checkpoint_ids = ids.clone();
        let checkpoints = f.world.with(system, move |s| checkpoint_ids.into_iter().map(|id| {
            assert!(s.set_condition_fraction(id, 0.5));
            s.combat_condition_snapshot(id).unwrap().condition
        }).collect::<Vec<_>>()).await.unwrap();
        let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut connection, driver) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
        row_driver = Some(tokio::spawn(async move { let _ = driver.await; }));
        let transaction = connection.transaction().await.unwrap();
        let blocker: i32 = transaction.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        transaction.query("SELECT item_id FROM game.items WHERE item_id=ANY($1) ORDER BY item_id FOR UPDATE", &[&ids]).await.unwrap();
        let args = [Value::list(ids.iter().copied().map(Value::Int))];
        let world = f.world.clone();
        let db = f.db.clone();
        let actor_ids = ids.clone();
        let mut call = Box::pin(f.call("ship", "ScoopDrone", &args));
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                tokio::select! {
                    value = &mut call => panic!("scoop completed before row unlock: {value:?}"),
                    _ = tokio::task::yield_now() => {}
                }
                if world.with(system, { let ids = actor_ids.clone(); move |s| ids.iter().all(|id| matches!(s.combat_phase(*id), Some(crate::space::combat::CombatPhase::Transition { .. }))) }).await.unwrap()
                    && db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.unwrap().get::<_, bool>("blocked") { break; }
            }
        }).await.expect("scoop transition not admitted");
        drop(call);
        let replacement = if replace_later {
            let id = ids[1];
            let prepared = super::super::restore_specs(&f.db, f.sde.clone(), system).await.ok().unwrap()
                .into_iter().find(|prepared| prepared.ball.id == id).unwrap();
            let effects = super::super::super::worker::effects(f.db.clone(), Arc::downgrade(&f.world), system);
            let first = ids[0];
            Some(f.world.with(system, move |s| {
                let old = s.combat_membership_generation(id).unwrap();
                s.remove_ball(id).unwrap();
                s.restore_drone(prepared, effects);
                let new = s.combat_membership_generation(id).unwrap();
                assert_ne!(old, new);
                assert!(s.ball(first).is_some() && matches!(s.combat_phase(first), Some(crate::space::combat::CombatPhase::Transition { .. })));
                new
            }).await.unwrap())
        } else { None };
        transaction.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(70), async {
            loop {
                let count: i64 = f.db.query_one("SELECT count(*) AS count FROM game.items WHERE item_id=ANY($1) AND location_id=$2 AND flag_id=$3", &[&ids, &ship, &FLAG_DRONE_BAY]).await.unwrap().get("count");
                let first = ids[0]; let last = ids[1];
                let settled = f.world.with(system, move |s| s.ball(first).is_none() && match replacement {
                    Some(generation) => s.ball(last).is_some() && s.combat_membership_generation(last) == Some(generation),
                    None => s.ball(last).is_none() && s.drone(last).is_none(),
                }).await.unwrap();
                if count == ids.len() as i64 && settled { break; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("owned scoop did not settle");
        assert!(frame.is_committed(&f.db).await.unwrap());
        for (&id, checkpoint) in ids.iter().zip(checkpoints.iter()) {
            let rows = crate::dao::items::dogma::overrides(&f.db, id).await.unwrap();
            let shield = rows.iter().find(|row| row.attribute_id == infinity_core::consts::dogma::ATTR_SHIELD_CHARGE).unwrap();
            let armor = rows.iter().find(|row| row.attribute_id == infinity_core::consts::dogma::ATTR_ARMOR_DAMAGE).unwrap();
            let hull = rows.iter().find(|row| row.attribute_id == infinity_core::consts::dogma::ATTR_HULL_DAMAGE).unwrap();
            assert!(shield.value >= checkpoint.now[0] && shield.updated_at >= checkpoint.shield_at);
            assert!((armor.value - (checkpoint.stats.max[1] - checkpoint.now[1])).abs() < 1e-6);
            assert!((hull.value - (checkpoint.stats.max[2] - checkpoint.now[2])).abs() < 1e-6);
        }
        }).await.expect("blocked scoop scenario timed out");
    }).await;
    if let Some(driver) = row_driver { driver.abort(); let _ = driver.await; }
    for id in fixture_ids {
        f.db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

type ForwardCommand = (bool, tokio::sync::oneshot::Sender<()>);

async fn forward_database() -> (crate::config::deployment::DatabaseConfig, tokio::sync::mpsc::UnboundedSender<ForwardCommand>, tokio::task::JoinHandle<()>) {
    let mut cfg = crate::config::tests::test_database();
    let parsed = cfg.to_pg_config().unwrap();
    let host = match parsed.get_hosts().first().expect("test database TCP host") {
        tokio_postgres::config::Host::Tcp(host) => host.clone(),
        _ => panic!("transport failure test requires a TCP test database"),
    };
    let port = parsed.get_ports().first().copied().unwrap_or(cfg.port);
    let user = parsed.get_user().expect("test database user").to_owned();
    let password = parsed.get_password().map(|p| String::from_utf8(p.to_vec()).expect("UTF-8 database password")).unwrap_or_default();
    let database = parsed.get_dbname().expect("test database name").to_owned();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    cfg.url.clear();
    cfg.host = address.ip().to_string();
    cfg.port = address.port();
    cfg.user = user;
    cfg.password = password;
    cfg.name = database;
    cfg.schema_on_start = false;
    cfg.connect_timeout_secs = 1;
    cfg.checkout_timeout_secs = 1;
    let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel::<ForwardCommand>();
    let task = tokio::spawn(async move {
        let mut enabled = true;
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some((online, ack)) = command else { break };
                    enabled = online;
                    if !enabled {
                        connections.abort_all();
                        while connections.join_next().await.is_some() {}
                    }
                    let _ = ack.send(());
                }
                accepted = listener.accept() => {
                    let Ok((mut incoming, _)) = accepted else { break };
                    if enabled {
                        let host = host.clone();
                        connections.spawn(async move {
                            if let Ok(mut outgoing) = tokio::net::TcpStream::connect((host.as_str(), port)).await {
                                let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                            }
                        });
                    }
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    });
    (cfg, sender, task)
}

async fn forward_online(sender: &tokio::sync::mpsc::UnboundedSender<ForwardCommand>, online: bool) {
    let (ack, received) = tokio::sync::oneshot::channel();
    sender.send((online, ack)).unwrap();
    received.await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn unavailable_return_reconciliation_keeps_transition_until_database_recovers() {
    let mut f = Fixture::new().await;
    let mut forwarder = None;
    let result = crate::tests::catch_scenario(async {
        tokio::time::timeout(std::time::Duration::from_secs(150), async {
        let (cfg, commands, task) = forward_database().await;
        forwarder = Some((commands.clone(), task));
        let db = Db::connect(&cfg).await.unwrap();
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).unwrap().type_id;
        f.train_drone(ty).await;
        let id = f.drone(ty, 1, true).await;
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        let launch = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
        assert_eq!(f.call("ship", "LaunchDrones", &launch).await.ok().unwrap(), Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]));
        let (system, ship, owner) = (f.system, f.ship, f.owner);
        let market = crate::services::market::lock().await;
        let gate = f.world.lock_condition_snapshot(system).await.unwrap();
        let fit = Fit::load_in_space(&f.db, &f.world, system, owner, ship).await.ok().unwrap();
        let (drone, transition) = f.world.with(system, move |s| {
            let drone = s.drone(id).unwrap();
            let transition = s.begin_condition_transitions(&[id]).unwrap();
            (drone, transition)
        }).await.unwrap();
        let generation = transition.entries[0].1.generation;
        forward_online(&commands, false).await;
        assert!(contact_return_claimed(&db, &f.world, system, &drone, ship, fit, transition, market, gate).await.is_err());
        let interval = u64::try_from(crate::config::game().space.position_snapshot_secs).unwrap();
        assert!(f.world.after(system, crate::space::seconds_to_ms(interval.saturating_add(2) as f64)).await);
        assert!(f.world.with(system, move |s| {
            matches!(s.combat_phase(id), Some(crate::space::combat::CombatPhase::Transition { .. }))
                && s.combat_membership_generation(id) == Some(generation)
                && s.pending_reconciliations().iter().any(|(item, _, _)| *item == id)
                && matches!(s.apply_damage(id, [1.0, 0.0, 0.0, 0.0], Default::default(), false), Ok(None))
        }).await.unwrap(), "unavailable DB blindly restored transition");
        forward_online(&commands, true).await;
        tokio::time::timeout(std::time::Duration::from_secs(interval.saturating_mul(2).saturating_add(10)), async {
            loop {
                if f.world.with(system, move |s| s.combat_membership_generation(id) == Some(generation)
                    && s.combat_phase(id) == Some(crate::space::combat::CombatPhase::Live)).await.unwrap() { break; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("unchanged drone did not recover after DB returned");
        }).await.expect("unavailable return scenario timed out");
    }).await;
    if let Some((commands, task)) = forwarder {
        let (ack, received) = tokio::sync::oneshot::channel();
        if commands.send((false, ack)).is_ok() { let _ = received.await; }
        drop(commands);
        let _ = task.await;
    }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn unavailable_group_scoop_survives_loss_of_first_pending_membership() {
    let mut f = Fixture::new().await;
    let mut fixture_ids = Vec::new();
    let mut row_driver = None;
    let mut forwarder = None;
    let result = crate::tests::catch_scenario(async {
        tokio::time::timeout(std::time::Duration::from_secs(150), async {
        let (cfg, commands, task) = forward_database().await;
        forwarder = Some((commands.clone(), task));
        let proxy_db = Db::connect(&cfg).await.unwrap();
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).unwrap().type_id;
        f.train_drone(ty).await;
        fixture_ids.push(f.drone(ty, 1, true).await);
        fixture_ids.push(f.drone(ty, 1, true).await);
        fixture_ids.sort_unstable();
        let ids = fixture_ids.clone();
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        let launch = [Value::list(ids.iter().map(|&id| Value::tuple([Value::Int(id), Value::Int(1)]))), Value::None, Value::Bool(false)];
        let expected = Value::Dict(ids.iter().map(|&id| (Value::Int(id), Value::list([Value::Int(id)]))).collect());
        assert_eq!(f.call("ship", "LaunchDrones", &launch).await.ok().unwrap(), expected);
        for &id in &ids { super::super::abandon(&mut f.client, id).await.ok().unwrap(); }
        let system = f.system;
        let (first, second) = (ids[0], ids[1]);
        let generation = f.world.with(system, move |s| s.combat_membership_generation(second).unwrap()).await.unwrap();
        let pg = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut connection, driver) = pg.connect(tokio_postgres::NoTls).await.unwrap();
        row_driver = Some(tokio::spawn(async move { let _ = driver.await; }));
        let transaction = connection.transaction().await.unwrap();
        let blocker: i32 = transaction.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
        transaction.query("SELECT item_id FROM game.items WHERE item_id=ANY($1) ORDER BY item_id FOR UPDATE", &[&ids]).await.unwrap();
        f.client.db = proxy_db;
        let args = [Value::list(ids.iter().copied().map(Value::Int))];
        let world = f.world.clone();
        let db = f.db.clone();
        let mut call = Box::pin(f.call("ship", "ScoopDrone", &args));
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                tokio::select! {
                    value = &mut call => panic!("group scoop completed before row unlock: {value:?}"),
                    _ = tokio::task::yield_now() => {}
                }
                if world.with(system, move |s| [first, second].iter().all(|id|
                    matches!(s.combat_phase(*id), Some(crate::space::combat::CombatPhase::Transition { .. })))).await.unwrap()
                    && db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.unwrap().get::<_, bool>("blocked") { break; }
            }
        }).await.expect("group scoop did not reach row lock");
        forward_online(&commands, false).await;
        drop(call);
        transaction.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if f.world.with(system, move |s| {
                    let pending = s.pending_reconciliations();
                    [first, second].iter().all(|id| pending.iter().any(|(item, _, _)| item == id))
                }).await.unwrap() { break; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("both group members must own pending reconciliation");
        let interval = u64::try_from(crate::config::game().space.position_snapshot_secs).unwrap();
        assert!(f.world.after(system, crate::space::seconds_to_ms(interval.saturating_add(2) as f64)).await);
        f.world.with(system, move |s| {
            s.remove_ball(first).unwrap();
            assert!(matches!(s.combat_phase(second), Some(crate::space::combat::CombatPhase::Transition { .. })));
            assert!(s.pending_reconciliations().iter().any(|(item, _, _)| *item == second));
        }).await.unwrap();
        forward_online(&commands, true).await;
        tokio::time::timeout(std::time::Duration::from_secs(interval.saturating_mul(2).saturating_add(10)), async {
            loop {
                if f.world.with(system, move |s| s.combat_membership_generation(second) == Some(generation)
                    && s.combat_phase(second) == Some(crate::space::combat::CombatPhase::Live)).await.unwrap() { break; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("remaining group member was stranded after recovery");
        let count: i64 = f.db.query_one("SELECT count(*) AS count FROM game.items WHERE item_id=ANY($1) AND location_id=$2 AND owner_id=$3", &[&ids, &system, &OWNER_UNOWNED]).await.unwrap().get("count");
        assert_eq!(count, 2);
        }).await.expect("unavailable group scoop scenario timed out");
    }).await;
    f.client.db = f.db.clone();
    if let Some(driver) = row_driver { driver.abort(); let _ = driver.await; }
    if let Some((commands, task)) = forwarder {
        let (ack, received) = tokio::sync::oneshot::channel();
        if commands.send((false, ack)).is_ok() { let _ = received.await; }
        drop(commands);
        let _ = task.await;
    }
    for id in fixture_ids {
        f.db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap();
        f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
    }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
fn committed_detached_contact_cold_load_does_not_relock_market() {
    use std::time::Duration;

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (mut f, result) = runtime.block_on(async {
        let mut f = Fixture::real_system().await;
        let mut driver = None;
        let result = crate::tests::catch_scenario(async {
            tokio::time::timeout(Duration::from_secs(90), async {
                let ty = f.sde.types.iter().find(|t| t.published == Some(true)
                    && t.category_id == Some(CATEGORY_DRONE)
                    && t.attribute(infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).unwrap().type_id;
                f.train_drone(ty).await;
                let id = f.drone(ty, 1, true).await;
                f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
                f.set(f.ship, infinity_core::consts::dogma::ATTR_DRONE_CAPACITY, 1_000_000.0).await;
                let launch = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
                assert_eq!(f.call("ship", "LaunchDrones", &launch).await.ok().unwrap(),
                    Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]));
                let system = f.system;
                let ship = f.ship;
                let owner = f.owner;
                let drone = f.world.with(system, move |s| s.drone(id).unwrap()).await.unwrap();
                assert_eq!(drone.controller, Some(ship));
                assert!(drone.pending.is_none());

                let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
                let (mut connection, connection_driver) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
                driver = Some(tokio::spawn(async move { let _ = connection_driver.await; }));
                let tx = connection.transaction().await.unwrap();
                let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
                tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&ship]).await.unwrap();

                let mut contact = Box::pin(contact_claimed(&f.db, &f.world, system, &drone, false));
                loop {
                    tokio::select! {
                        result = &mut contact => panic!("contact finished before row unlock: {result:?}"),
                        _ = tokio::task::yield_now() => {}
                    }
                    if f.db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.unwrap().get::<_, bool>("blocked") { break; }
                }
                f.world.post(system, |_| panic!("deliberate actor stop for committed contact regression"));
                loop {
                    if f.world.with(system, |_| ()).await.is_none() { break; }
                    tokio::task::yield_now().await;
                }
                tx.commit().await.unwrap();
                tokio::time::timeout(Duration::from_secs(30), &mut contact).await
                    .expect("committed contact cold fallback deadlocked").ok().unwrap();
                let row = f.db.query_one("SELECT owner_id, location_id, quantity, singleton FROM game.items WHERE item_id=$1", &[&id]).await.unwrap();
                assert_eq!(row.get::<_, i64>("owner_id"), owner);
                assert_eq!(row.get::<_, i64>("location_id"), system);
                assert_eq!(row.get::<_, i64>("quantity"), 1);
                assert!(row.get::<_, bool>("singleton"));
                assert!(f.world.with(system, move |s| s.ball(id).is_some()
                    && s.drone(id).is_some_and(|d| d.owner == owner && d.controller.is_none() && d.pending.is_none())).await.unwrap());
                let guard = tokio::time::timeout(Duration::from_secs(10), crate::services::market::lock()).await
                    .expect("market lock retained after contact");
                drop(guard);
            }).await.expect("contact regression scenario timed out");
        }).await;
        if let Some(driver) = driver { driver.abort(); let _ = driver.await; }
        (f, result)
    });
    runtime.shutdown_timeout(Duration::from_secs(5));

    let cleanup_runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    cleanup_runtime.block_on(async {
        f.db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        f.world = World::new(f.db.clone(), f.sde.clone(), 1);
        f.client.db = f.db.clone();
        f.client.world = f.world.clone();
        f.world.system(f.system).await.unwrap();
        f.cleanup().await;
    });
    cleanup_runtime.shutdown_timeout(Duration::from_secs(5));
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}