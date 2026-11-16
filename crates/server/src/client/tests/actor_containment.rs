//! One failed system retires its exact pilots; another system keeps playing.
use super::{panic_containment::{call_req, response, select, session}, *};

const WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Create a hull row at `pos` and select it as the character's ship.
async fn ship(db: &Db, owner: i64, system: i64, kind: i32, pos: [f64; 3]) -> i64 {
    let id = crate::dao::items::creation::positioned(db, kind, owner, system, pos).await.unwrap();
    crate::dao::characters::selection::set_ship(db, owner, id).await.unwrap();
    id
}

async fn position(db: &Db, ship: i64) -> [f64; 3] {
    let row = db.query_one("SELECT x, y, z FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    [row.get("x"), row.get("y"), row.get("z")]
}

/// Wait until the actor's memory reports `pos` for `ship`.
async fn await_ball_pos(world: &crate::space::World, system: i64, ship: i64, pos: [f64; 3]) {
    tokio::time::timeout(WAIT, async {
        loop {
            let actual = world.with(system, move |s| s.ball(ship).map(|ball| ball.pos)).await.flatten();
            if actual == Some(pos) { return; }
            tokio::task::yield_now().await;
        }
    }).await.expect("in-memory ball position was not applied");
}

async fn scenario(db: Db, sde: Arc<crate::sde::Sde>, world: Arc<crate::space::World>, cfg: Arc<Config>,
    fixtures: [crate::services::tests::fixture::Fixture; 4], kind: i32)
{
    let first: i64 = db.query_one("SELECT solar_system_id FROM game.characters WHERE character_id=$1", &[&fixtures[0].character])
        .await.unwrap().get("solar_system_id");
    // C's durable location is a different public system so its actor survives.
    let second_row = sde.map.systems()
        .find(|row| row.solar_system_id != first && row.constellation_id.is_some() && row.region_id.is_some()
            && sde.map.is_public_system(row.solar_system_id))
        .expect("a second public system with full ancestry");
    let second = second_row.solar_system_id;
    crate::dao::characters::ship::location(&db, fixtures[2].character, second,
        second_row.constellation_id.unwrap(), second_row.region_id.unwrap()).await.unwrap();
    // Literal spots far outside every solar system's celestial field (the
    // SDE keeps every celestial within ~3.5e13 m of the origin) and far
    // enough apart that no fixture hull's radius can touch a neighbour.
    let spots = [[1.0e15, 0.0, 0.0], [1.0e15, 1.0e6, 0.0], [-1.0e15, 0.0, 0.0], [-1.0e15, 1.0e6, 0.0]];
    let mut placed = [0; 4];
    for (index, fixture) in fixtures.iter().enumerate() {
        placed[index] = ship(&db, fixture.character, if index == 2 { second } else { first }, kind, spots[index]).await;
    }
    let [a, b, c, d] = fixtures;
    let (mut peer_a, run_a) = session(&db, &sde, &world, &cfg, a.account).await;
    let (mut peer_b, run_b) = session(&db, &sde, &world, &cfg, b.account).await;
    let (mut peer_c, run_c) = session(&db, &sde, &world, &cfg, c.account).await;
    select(&mut peer_a, a.account, a.character, 1).await;
    select(&mut peer_b, b.account, b.character, 1).await;
    select(&mut peer_c, c.account, c.character, 1).await;
    assert_eq!(world.with(first, move |s| s.pilots().count()).await, Some(2));
    assert_eq!(world.with(second, move |s| s.pilots().count()).await, Some(1));

    // Establish A's saved baseline through the real position flush, then hold
    // the snapshot gate so the periodic writer cannot refresh it again while
    // the in-memory ball diverges.
    world.flush_positions().await.unwrap();
    let saved = position(&db, placed[0]).await;
    let gate = world.lock_condition_snapshot(first).await.unwrap();
    let ship_a = placed[0];
    let dirty = [1.0e15, -1.0e6, 0.0];
    world.with(first, move |s| s.set_ball_position(ship_a, dirty, |_, _| true)).await.unwrap();
    await_ball_pos(&world, first, ship_a, dirty).await;
    assert_eq!(position(&db, placed[0]).await, saved, "held snapshot gate must keep the saved baseline");

    // Admitted work on the old worker: a real deferred transaction holding the
    // market guard, paused on a oneshot inside the owned worker.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let ship_b = placed[1];
    let committed = [1.0e15, 2.0e6, 0.0];
    let db_job = db.clone();
    world.with(first, move |system| system.defer(async move {
        let market = crate::services::market::lock().await;
        let _ = ready_tx.send(());
        if release_rx.await.is_err() { return; }
        db_job.transaction(|tx, _: &mut ()| Box::pin(async move {
            crate::dao::items::ship::position(tx, ship_b, first, committed).await.map_err(crate::services::db_err)
        }), |_, ()| async { Ok(()) }).await.unwrap();
        drop(market);
    })).await.unwrap();
    ready_rx.await.expect("admitted worker never reached its market guard");

    // Capture the exact old tokens before the failure.
    let stale = world.with(first, move |system| {
        let (_, binding) = system.pilot_bindings().find(|(character, _)| *character == a.character)?;
        binding.and_then(|binding| match binding.target() {
            Some(crate::space::containment::DisconnectTarget::Tcp(target)) => Some(target.clone()),
            _ => None,
        })
    }).await.unwrap().expect("A's membership must expose its failure target");
    let old = world.admission(first).unwrap();

    // A real mailbox closure panics inside the failed actor.
    world.post(first, |_| panic!("injected actor panic"));

    // While the admitted worker still holds the market, the tombstone refuses
    // a new entry without waiting on that guard, and the other system plays on.
    tokio::time::timeout(WAIT, async {
        while world.admission(first).is_ok() { tokio::task::yield_now().await; }
    }).await.expect("failed actor never became a tombstone");
    let (mut peer_d, run_d) = session(&db, &sde, &world, &cfg, d.account).await;
    let d_call = Address::Client { client_id: d.account, call_id: Value::Int(1), service: None };
    peer_d.send(&call_req(d_call, d.account, "charUnboundMgr", "SelectCharacterID", vec![Value::Int(d.character)]).to_value()).await.unwrap();
    tokio::time::timeout(WAIT, async {
        while let Some(value) = peer_d.recv().await.unwrap() {
            assert_ne!(value, infinity_wire::conn::usurped_close(), "retired entry must not look like a takeover");
        }
        let retiring = run_d.await.expect_err("a new entry into the failed system must end the session");
        assert!(retiring.downcast_ref::<crate::space::containment::ActorRetiring>().is_some(),
            "new entry did not end with the typed retirement: {retiring:#}");
    }).await.expect("retired entry did not end while the admitted worker held the market");
    let cohorts = Address::Client { client_id: c.account, call_id: Value::Int(2), service: None };
    peer_c.send(&call_req(cohorts.clone(), c.account, "charUnboundMgr", "GetCohortsForUser", vec![]).to_value()).await.unwrap();
    assert_eq!(response(&mut peer_c, &cohorts).await.command, cmd::CALL_RSP);
    assert!(world.with(first, |_| ()).await.is_none(), "failed actor must not serve new work");

    // Release the admitted worker; its commit must land before the cold load.
    drop(gate);
    release_tx.send(()).unwrap();
    let (cleanup_a, cleanup_b) = (run_a.await, run_b.await);
    let error_a = cleanup_a.expect_err("A's missing actor must not report a successful cleanup");
    let error_b = cleanup_b.expect_err("B's missing actor must not report a successful cleanup");
    tracing::info!(%error_a, %error_b, "retired sessions recorded their actual cleanup errors");
    let mut online = world.online_pilots();
    online.sort_unstable();
    assert_eq!(online, vec![c.character], "only the surviving system's pilot stays online");
    assert!(world.online_session(a.character).is_none() && world.online_session(b.character).is_none());

    // The tombstone is removed only after every proof; removal is not a restart.
    tokio::time::timeout(WAIT, async {
        loop {
            if world.admission(first).is_ok() && world.with(first, |_| ()).await.is_none() { return; }
            tokio::task::yield_now().await;
        }
    }).await.expect("failed actor tombstone was never removed");

    // Fresh sessions cold-load the last committed state.
    let (mut again_a, again_run_a) = session(&db, &sde, &world, &cfg, a.account).await;
    let (mut again_b, again_run_b) = session(&db, &sde, &world, &cfg, b.account).await;
    select(&mut again_a, a.account, a.character, 3).await;
    select(&mut again_b, b.account, b.character, 3).await;

    // A late stop from the dead generation cannot cancel the new session.
    stale.fail_actor();
    let call = Address::Client { client_id: a.account, call_id: Value::Int(4), service: None };
    again_a.send(&call_req(call.clone(), a.account, "charUnboundMgr", "GetCohortsForUser", vec![]).to_value()).await.unwrap();
    assert_eq!(response(&mut again_a, &call).await.command, cmd::CALL_RSP);

    assert_eq!(position(&db, placed[0]).await, saved, "A must cold-load the saved baseline, not damaged memory");
    assert_eq!(position(&db, placed[1]).await, committed, "B must see the admitted in-flight commit");
    let fresh_a = placed[0];
    let ball_a = world.with(first, move |s| s.ball(fresh_a).map(|ball| ball.pos)).await.flatten();
    let fresh_b = placed[1];
    let ball_b = world.with(first, move |s| s.ball(fresh_b).map(|ball| ball.pos)).await.flatten();
    assert_eq!(ball_a, Some(saved), "A's fresh actor ball must be the saved baseline");
    assert_eq!(ball_b, Some(committed), "B's fresh actor ball must be the committed position");
    assert!(old.check().is_err(), "a captured admission stays refused after the tombstone is gone");

    drop((peer_a, peer_b, peer_c, peer_d, again_a, again_b));
    let _ = (run_c.await, again_run_a.await, again_run_b.await);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn failed_actor_retires_exact_pilots_and_waits_for_committed_work() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 0);
    let kind = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let fixtures = [crate::services::tests::fixture::create(&db).await.unwrap(), crate::services::tests::fixture::create(&db).await.unwrap(), crate::services::tests::fixture::create(&db).await.unwrap(), crate::services::tests::fixture::create(&db).await.unwrap()];
    let result = tokio::spawn(scenario(db.clone(), sde, world, Arc::new(Config::default()), fixtures, kind)).await;
    for fixture in fixtures {
        let mut barrier = crate::live_sessions::register(fixture.account, true).unwrap();
        barrier.wait_for_predecessor().await;
        drop(barrier);
        crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    }
    result.unwrap();
}

/// `MachoBindObject(params, (method, args, kwargs))` over the wire: the server
/// binds the service moniker, runs the inner bound call and answers
/// `(object, inner result)`. Returns the inner result.
async fn bind_call(peer: &mut Conn, account: i64, service: &str, params: Value, method: &str, args: Vec<Value>, call_id: i64) -> Value {
    let source = Address::Client { client_id: account, call_id: Value::Int(call_id), service: None };
    let inner = Value::Tuple(vec![Value::str(method), Value::Tuple(args), Value::Dict(vec![])]);
    peer.send(&call_req(source.clone(), account, service, "MachoBindObject", vec![params, inner]).to_value()).await.unwrap();
    let packet = response(peer, &source).await;
    assert_eq!(packet.command, cmd::CALL_RSP, "MachoBindObject {service}::{method} refused");
    let bound = marshal::loads(packet.body.iter().find_map(|v| v.as_bytes()).expect("bind response carries no payload"))
        .expect("bind response payload is not marshal");
    bound.items().unwrap()[1].clone()
}

// SDE dgmAttributeTypes used by the shared mining calculation.
const MINING_AMOUNT: i32 = 77;
const WASTE_PROBABILITY: i32 = 3154;
const CRIT_CHANCE: i32 = 5967;

async fn contact_scenario(db: Db, sde: Arc<crate::sde::Sde>, world: Arc<crate::space::World>, cfg: Arc<Config>,
    fixture: crate::services::tests::fixture::Fixture, system: i64, ship: i64, drone: i64, drone_type: i32,
    duration_attr: i32, range_attr: i32)
{
    use infinity_core::consts::dogma::*;
    let owner = fixture.character;
    let account = fixture.account;

    // Skills for the real drone launch path: Drones at five plus the type's
    // declared required skills; the fit reads the level column.
    let drone_ty = sde.types.get(drone_type).unwrap();
    let mut levels = std::collections::BTreeMap::from([(sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SKILL)
        && t.effects().iter().any(|e| e.effect_id == 316)).expect("Drones skill effect").type_id, 5i32)]);
    for (&skill_attr, &level_attr) in ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS.iter()) {
        let id = drone_ty.attribute(skill_attr).unwrap_or(0.0) as i32;
        let level = drone_ty.attribute(level_attr).unwrap_or(0.0) as i32;
        if id > 0 { levels.entry(id).and_modify(|v| *v = (*v).max(level)).or_insert(level); }
    }
    for (skill, level) in levels {
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4)",
            &[&owner, &skill, &level, &i64::MAX]).await.unwrap();
    }

    let (mut peer, run) = session(&db, &sde, &world, &cfg, account).await;
    select(&mut peer, account, owner, 1).await;
    // The fixture is station-bound; real ship.Undock is the only way out.
    let undock = Address::Client { client_id: account, call_id: Value::Int(2), service: None };
    peer.send(&call_req(undock.clone(), account, "ship", "Undock", vec![]).to_value()).await.unwrap();
    assert_eq!(response(&mut peer, &undock).await.command, cmd::CALL_RSP);
    // Own field on the public grow path, one axis off the actual post-undock
    // ship ball; its rows live under this test's exact (system, field_id) key.
    let (ship_pos, ship_radius) = world.with(system, move |s| s.ball(ship).map(|b| (b.pos, b.radius))).await.flatten().expect("ship has no ball after undock");
    let rocks = crate::services::mining::grow_field(&db, &world, system, ship,
        [ship_pos[0] + f64::from(ship_radius) + 1_000.0, ship_pos[1], ship_pos[2]], crate::space::asteroids::Field::Ore, 1.0, false).await.unwrap();
    let (rock, gap) = world.with(system, move |s| rocks.iter().filter_map(|&id| s.ball(id)
        .map(|b| (id, crate::space::dist(b.pos, ship_pos) - f64::from(ship_radius) - f64::from(b.radius))))
        .filter(|&(_, gap)| gap > 250.0).min_by(|a, b| a.1.total_cmp(&b.1)))
        .await.unwrap().expect("no non-overlapping own rock");
    let optimal = (gap + 1_000.0).max(500.0);
    assert!(gap > 250.0 && gap <= optimal, "own rock must sit non-overlapping inside mining range: gap={gap} optimal={optimal}");
    let ore = world.with(system, move |s| s.ball(rock).map(|b| b.type_id)).await.flatten().expect("grown rock has no ball");
    let volume = sde.types.get(ore).and_then(|t| t.volume).unwrap_or(1.0).max(0.01);
    for (item, attr, value) in [
        (ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0), (ship, ATTR_DRONE_CAPACITY, 1_000_000.0),
        (ship, ATTR_MAX_TARGET_RANGE, 1_000_000.0), (ship, ATTR_SCAN_RESOLUTION, 1_000_000.0),
        (ship, ATTR_MAX_LOCKED_TARGETS, 10.0), (ship, ATTR_GENERAL_MINING_HOLD_CAPACITY, 0.0),
        (ship, ATTR_CAPACITY, volume * 100.0),
        (drone, ATTR_MAX_VELOCITY, 10_000.0), (drone, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
        (drone, ATTR_SCAN_RESOLUTION, 1_000_000.0), (drone, ATTR_MAX_RANGE, optimal),
        (drone, range_attr, optimal), (drone, duration_attr, 1_000.0),
        (drone, MINING_AMOUNT, volume * 2.0), (drone, WASTE_PROBABILITY, 0.0), (drone, CRIT_CHANCE, 0.0),
    ] {
        assert!(crate::dao::items::dogma::set_override(&db, item, attr, value).await.unwrap(), "override requires a real singleton item");
    }

    // Bound ship service then bound entity moniker drive the real cycle.
    assert_eq!(bind_call(&mut peer, account, "ship", Value::tuple([Value::Int(system), Value::Int(5)]), "LaunchDrones",
        vec![Value::list([Value::tuple([Value::Int(drone), Value::Int(1)])]), Value::None, Value::Bool(false)], 3).await,
        Value::Dict(vec![(Value::Int(drone), Value::list([Value::Int(drone)]))]));
    let record = world.with(system, move |s| s.drone(drone)).await.unwrap().expect("launched drone record");
    assert_eq!(record.controller, Some(ship));
    assert!(record.pending.is_none());

    let target = Address::Client { client_id: account, call_id: Value::Int(4), service: None };
    peer.send(&call_req(target.clone(), account, "dogmaIM", "AddTarget", vec![Value::Int(rock)]).to_value()).await.unwrap();
    assert_eq!(response(&mut peer, &target).await.command, cmd::CALL_RSP);
    tokio::time::timeout(WAIT, async {
        while !crate::services::combat::targeting_charges::targets_of(owner).contains(&rock) {
            assert!(world.after(system, 100).await);
        }
    }).await.expect("asteroid lock timed out");
    assert_eq!(bind_call(&mut peer, account, "entity", Value::Int(system), "CmdMineRepeatedly",
        vec![Value::list([Value::Int(drone)]), Value::Int(rock)], 5).await, Value::Dict(vec![]));

    // The public cycle must reach a current-generation pending intent.
    let generation = tokio::time::timeout(WAIT, async {
        loop {
            let found = world.with(system, move |s| s.drone(drone).map(|d| (d.generation,
                d.pending.is_some() && d.order == crate::space::drone::DroneOrder::Mine(rock) && d.controller == Some(ship)))).await;
            match found {
                Some(Some((generation, true))) => break generation,
                Some(_) => assert!(world.after(system, 100).await, "actor stopped while mining"),
                None => panic!("actor stopped while mining"),
            }
        }
    }).await.expect("current-generation mining intent never formed");

    // Lock only the original ship row: the owned worker's contact transaction
    // must reach prepare_ship's FOR UPDATE behind this test's own backend PID.
    let config = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut connection, connection_driver) = config.connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move { let _ = connection_driver.await; });
    let tx = connection.transaction().await.unwrap();
    let blocker: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await.unwrap().get("pid");
    tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&ship]).await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            match world.with(system, move |s| s.drone_owns_contact(drone, generation)).await {
                Some(true) => {
                    let blocked: bool = db.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await.unwrap().get("blocked");
                    if blocked { break; }
                }
                Some(_) => {}
                None => panic!("actor stopped before the native contact claim"),
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("native Mine contact never queued behind the ship-row lock");

    // The real panic lands only after the claim queues behind the lock.
    world.post(system, |_| panic!("deliberate actor stop for committed contact regression"));
    tokio::time::timeout(WAIT, async {
        while world.admission(system).is_ok() { tokio::task::yield_now().await; }
    }).await.expect("failed actor never became a tombstone");
    // Retirement joins the owned worker — never wait while the blocker holds
    tx.commit().await.unwrap();
    driver.abort();
    let _ = driver.await;
    assert!(run.await.is_err(), "retired session must not report a successful cleanup");
    assert!(world.online_session(owner).is_none() && world.online_pilots().is_empty());
    tokio::time::timeout(WAIT, async {
        loop {
            if world.admission(system).is_ok() && world.with(system, |_| ()).await.is_none() { return; }
            tokio::task::yield_now().await;
        }
    }).await.expect("failed actor tombstone was never removed");

    // Committed facts and the market survive the contact's cold fallback.
    let row = db.query_one("SELECT owner_id, location_id, quantity, singleton FROM game.items WHERE item_id=$1", &[&drone]).await.unwrap();
    assert_eq!(row.get::<_, i64>("owner_id"), owner);
    assert_eq!(row.get::<_, i64>("location_id"), system);
    assert_eq!(row.get::<_, i64>("quantity"), 1);
    assert!(row.get::<_, bool>("singleton"));
    let guard = tokio::time::timeout(std::time::Duration::from_secs(10), crate::services::market::lock()).await.expect("market lock retained after contact");
    drop(guard);

    // A genuine fresh session cold-loads the committed detached drone.
    let (mut again, again_run) = session(&db, &sde, &world, &cfg, account).await;
    select(&mut again, account, owner, 7).await;
    let record = world.with(system, move |s| s.drone(drone)).await.unwrap().expect("cold-loaded drone record");
    assert!(record.owner == owner && record.controller.is_none() && record.pending.is_none()
        && record.order == crate::space::drone::DroneOrder::Idle);
    drop((peer, again));
    let _ = again_run.await;
}

// MR/test note: the private-path pendingNone-at-contact-claim interleaving is
// no longer covered — a public native (non-bay) contact requires a real
// pending mining intent, while ReturnBay reports true. The launch pendingNone
// and committed detached-contact/no-relock assertions are preserved.
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn committed_detached_contact_cold_load_does_not_relock_market() {
    use infinity_core::consts::dogma::*;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 0);
    let kind = sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let (drone_type, duration_attr, range_attr) = sde.types.iter().filter(|t| t.name.as_deref() == Some("Mining Drone I")
        && t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_DRONE)
        && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)).find_map(|t| {
        t.effects().iter().filter_map(|e| crate::services::effects::registry::resolve(&sde, e.effect_id))
            .find(|e| e.kind == crate::services::effects::EffectKind::Mining)
            .map(|e| (t.type_id, e.attribute(crate::services::effects::registry::EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE),
                e.attribute(crate::services::effects::registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE)))
    }).expect("published mining drone unavailable");
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let system: i64 = db.query_one("SELECT solar_system_id FROM game.characters WHERE character_id=$1", &[&fixture.character]).await.unwrap().get("solar_system_id");
    let station = sde.map.stations().find(|s| s.solar_system_id == Some(system)).expect("fixture system has a station").station_id;
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&fixture.character, &station]).await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&db, kind, fixture.character, station, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    crate::dao::characters::selection::set_ship(&db, fixture.character, ship).await.unwrap();
    let drone: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id",
        &[&drone_type, &fixture.character, &ship, &infinity_core::consts::FLAG_DRONE_BAY]).await.unwrap().get("item_id");
    let result = tokio::spawn(contact_scenario(db.clone(), sde, world, Arc::new(Config::default()), fixture, system, ship, drone,
        drone_type, duration_attr, range_attr)).await;
    // Exact-own-ID cleanup runs on every path before the fixture rows go.
    db.execute("DELETE FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&system, &ship]).await.unwrap();
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&system, &ship]).await.unwrap();
    db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1 AND source_ship_id=$2 AND solar_system_id=$3",
        &[&fixture.character, &ship, &system]).await.unwrap();
    db.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&vec![ship, drone]]).await.unwrap();
    let mut barrier = crate::live_sessions::register(fixture.account, true).unwrap();
    barrier.wait_for_predecessor().await;
    drop(barrier);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
