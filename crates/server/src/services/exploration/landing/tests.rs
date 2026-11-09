use super::*;

#[test]
fn reservation_is_once_and_failure_only_releases_empty() {
    let mut slot = None;
    assert!(reserve(&mut slot));
    assert!(!reserve(&mut slot));
    release(&mut slot);
    assert!(reserve(&mut slot));
    slot = Some(vec![42]);
    release(&mut slot);
    assert_eq!(slot, Some(vec![42]));
    assert!(!reserve(&mut slot));
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) { self.0.abort(); }
}

async fn notification(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>, expected: &str, phase: &str) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(90), rx.recv()).await
        .unwrap_or_else(|_| panic!("notification timeout waiting for {expected} ({phase})"))
        .unwrap_or_else(|| panic!("listener stopped waiting for {expected} ({phase})"))
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn failed_site_save_releases_reservation_and_retry_reuses_committed_rocks() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = *sde.dungeons.systems_of_class(7).first().expect("known-space system");
    let mut site = new_site(system, [0.0; 3], Kind::Anomaly, STRENGTH_GRAVIMETRIC, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    site.archetype = Some(ARCHETYPE_ORE);
    let site_id = site.id;
    let trigger = format!("test_landing_failure_{site_id}");
    let test_trigger = trigger.clone();
    let result = tokio::task::spawn_blocking(move || {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    runtime.block_on(async move {
    let db = Db::connect(&config).await.unwrap();
    let world = World::new(db.clone(), sde, 0);
    world.system(system).await.unwrap();
    let trigger = test_trigger;
    save_site(&db, &site).await.unwrap();
    state().sites.insert(site.id, site.clone());
    db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'site save failure'; END $$"), &[]).await.unwrap();
    db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.exploration_sites FOR EACH ROW WHEN (NEW.site_id = {}) EXECUTE FUNCTION game.{trigger}()", site.id), &[]).await.unwrap();
    assert!(arrive(&db, &world, 1, system, site.id).await.is_err());
    assert!(state().sites.get(&site.id).unwrap().containers.is_none());
    let rows = db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2 ORDER BY asteroid_id", &[&system, &site.id]).await.unwrap();
    let ids: Vec<i64> = rows.iter().map(|row| row.get("asteroid_id")).collect();
    assert!(!ids.is_empty());
    db.execute(&format!("DROP TRIGGER {trigger} ON game.exploration_sites"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION game.{trigger}()"), &[]).await.unwrap();
    arrive(&db, &world, 1, system, site.id).await.unwrap();
    let mut actual = state().sites.get(&site.id).unwrap().containers.clone().unwrap();
    actual.sort_unstable();
    assert_eq!(actual, ids);
    let rows = db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2 ORDER BY asteroid_id", &[&system, &site.id]).await.unwrap();
    assert_eq!(rows.iter().map(|row| row.get::<_, i64>("asteroid_id")).collect::<Vec<_>>(), ids);
    let stored = crate::dao::exploration::sites(&db, system).await.unwrap();
    let mut persisted = stored.into_iter().find(|s| s.id == site.id).unwrap().containers.unwrap();
    persisted.sort_unstable();
    assert_eq!(persisted, ids);
    let actor = world.asteroid_actor(system).expect("fixture asteroid actor");
    for &rock in &ids { assert_eq!(actor.rock_field(rock).await, Some(site.id)); }
    });
    }).await;
    db.execute(&format!("DROP TRIGGER IF EXISTS {trigger} ON game.exploration_sites"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS game.{trigger}()"), &[]).await.unwrap();
    state().sites.remove(&site_id);
    delete_site(&db, site_id).await.unwrap();
    let rows = db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&system, &site_id]).await.unwrap();
    for row in rows {
        let rock: i64 = row.get("asteroid_id");
        db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &system, &site_id]).await.unwrap();
    }
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&system, &site_id]).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn scan_arrival_finishes_after_client_drop_and_tracks_clearance() {
    use infinity_core::consts::dogma::{ATTR_BASE_WARP_SPEED, ATTR_WARP_SPEED_MULTIPLIER};

    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = *sde.dungeons.systems_of_class(7).first().expect("known-space system");
    let map = sde.map.system(system).expect("system map row");
    let (constellation, region) = (map.constellation_id.unwrap(), map.region_id.unwrap());
    let ship_type = sde.types.iter().find(|t| {
        t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)
            && t.attribute(ATTR_BASE_WARP_SPEED).is_some_and(|v| v > 0.0 && v.is_finite())
            && t.attribute(ATTR_WARP_SPEED_MULTIPLIER).is_some_and(|v| v > 0.0 && v.is_finite())
    }).expect("SDE ship with warp attributes").type_id;
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let baseline: HashSet<i64> = crate::dao::exploration::sites(&db, system).await.unwrap().into_iter().map(|s| s.id).collect();
    let mut taken = system_sites(system).into_iter().map(|s| s.target).collect();
    let mut site = new_site(system, [1_000_000.0, 0.0, 0.0], Kind::Anomaly, STRENGTH_GRAVIMETRIC, 1.0, &mut taken);
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    site.archetype = Some(ARCHETYPE_ORE);
    let site_id = site.id;
    let prefix = format!("landing_test_{site_id}");
    let test_prefix = prefix.clone();
    let result = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
        let test_db = Db::connect(&config).await.unwrap();
        let test_world = World::new(test_db.clone(), sde.clone(), 1);
        test_world.system(system).await.unwrap();
        let celestials: Vec<_> = sde.map.celestials(system).filter_map(|c| {
            Some((c.item_id, [c.x?, c.y?, c.z?], f64::from(sde.celestial_radius(c))))
        }).collect();
        let (origin, center) = test_world.with(system, move |s| {
            celestials.into_iter().find_map(|(id, center, radius)| {
                let origin = s.warp_in(id)?;
                let distance = crate::space::dist(origin, center);
                (distance.is_finite() && distance > radius).then_some((origin, center))
            })
        }).await.unwrap().expect("SDE celestial with safe canonical warp-in");
        let outward = crate::space::sub(origin, center);
        site.pos = crate::space::add(origin, crate::space::scale(outward, 1_000_000.0 / crate::space::dist(origin, center)));
        let ship = crate::dao::items::creation::positioned(&test_db, ship_type, fixture.character, system, origin).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2, solar_system_id=$3, station_id=NULL, constellation_id=$4, region_id=$5 WHERE character_id=$1",
            &[&fixture.character, &ship, &system, &constellation, &region]).await.unwrap();
        let row = crate::dao::items::space_ships::FreeShip { item_id: ship, type_id: ship_type, owner_id: fixture.character,
            corporation_id: 1, alliance_id: -1, pos: origin };
        let prepared = crate::services::ship::balls::piloted(&test_db, sde.clone(), &row, fixture.character, 0.0).await.ok().unwrap();
        test_world.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let (pilot_tx, mut pilot_rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(fixture.character, pilot_tx);
        save_site(&test_db, &site).await.unwrap();
        state().sites.insert(site_id, site.clone());

        let (listener, mut connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _listener_task = AbortOnDrop(tokio::spawn(async move {
            while let Some(message) = std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                match message {
                    Ok(tokio_postgres::AsyncMessage::Notification(n)) => { let _ = tx.send(n.channel().to_owned()); }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }));
        for suffix in ["rocks", "saved", "cleared"] {
            listener.batch_execute(&format!("LISTEN {test_prefix}_{suffix}")).await.unwrap();
        }
        for (suffix, table, event, predicate, row) in [
            ("rocks", "asteroids", "INSERT", format!("NEW.field_id = {site_id}"), "NEW"),
            ("saved", "exploration_sites", "INSERT OR UPDATE", format!("NEW.site_id = {site_id} AND NEW.containers IS NOT NULL"), "NEW"),
            ("cleared", "exploration_sites", "DELETE", format!("OLD.site_id = {site_id}"), "OLD"),
        ] {
            let name = format!("{test_prefix}_{suffix}");
            test_db.execute(&format!("CREATE FUNCTION game.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_notify('{name}', ''); RETURN {row}; END $$"), &[]).await.unwrap();
            test_db.execute(&format!("CREATE TRIGGER {name} AFTER {event} ON game.{table} FOR EACH ROW WHEN ({predicate}) EXECUTE FUNCTION game.{name}()"), &[]).await.unwrap();
        }
        let (mut gate, gate_connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
        let _gate_task = AbortOnDrop(tokio::spawn(async move { let _ = gate_connection.await; }));
        let transaction = gate.transaction().await.unwrap();
        transaction.query_one("SELECT site_id FROM game.exploration_sites WHERE site_id=$1 FOR UPDATE", &[&site_id]).await.unwrap();
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(socket.local_addr().unwrap()).await.unwrap();
        let (stream, address) = socket.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), test_world.clone());
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "landing-fixture".into(),
            role: 0, client_id: fixture.account, session_id: fixture.account, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde, chat, test_world.clone(), login);
        client.session.apply(vec![("userid", Value::Int(fixture.account)), ("charid", Value::Int(fixture.character)),
            ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system)),
            ("constellationid", Value::Int(constellation)), ("regionid", Value::Int(region)), ("corpid", Value::Int(1))]);
        super::super::warp_to_scan(&mut client, &site.target, 0.0, None).await.ok().expect("scan warp");
        drop(client);
        drop(peer);
        assert_eq!(notification(&mut rx, &format!("{test_prefix}_rocks"), "rocks committed").await, format!("{test_prefix}_rocks"));
        let rows = test_db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2 ORDER BY asteroid_id", &[&system, &site_id]).await.unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.get("asteroid_id")).collect();
        assert!(!ids.is_empty());
        transaction.commit().await.unwrap();
        assert_eq!(notification(&mut rx, &format!("{test_prefix}_saved"), "site saved").await, format!("{test_prefix}_saved"));
        let mut stored = crate::dao::exploration::sites(&test_db, system).await.unwrap().into_iter()
            .find(|s| s.id == site_id).unwrap().containers.unwrap();
        stored.sort_unstable();
        assert_eq!(stored, ids);
        let actor = test_world.asteroid_actor(system).unwrap();
        for &id in &ids {
            assert_eq!(actor.rock_field(id).await, Some(site_id));
            assert!(test_world.ball(system, id).await.is_some());
        }
        for &id in &ids { test_world.remove_ball(system, id).await.unwrap(); }
        assert_eq!(notification(&mut rx, &format!("{test_prefix}_cleared"), "old site cleared").await, format!("{test_prefix}_cleared"));
        assert!(!crate::dao::exploration::sites(&test_db, system).await.unwrap().iter().any(|s| s.id == site_id));
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            while let Some((event, _)) = pilot_rx.recv().await {
                if event == "OnSignalTrackerAnomalyUpdate" { return; }
            }
            panic!("pilot notification channel closed");
        }).await.expect("replacement broadcast timeout");
        assert!(!state().sites.contains_key(&site_id));
        drop(listener);
        drop(gate);
        });
    }).await;

    for (suffix, table) in [("rocks", "asteroids"), ("saved", "exploration_sites"), ("cleared", "exploration_sites")] {
        let name = format!("{prefix}_{suffix}");
        db.execute(&format!("DROP TRIGGER IF EXISTS {name} ON game.{table}"), &[]).await.unwrap();
        db.execute(&format!("DROP FUNCTION IF EXISTS game.{name}()"), &[]).await.unwrap();
    }
    let remaining = crate::dao::exploration::sites(&db, system).await.unwrap();
    for row in remaining.into_iter().filter(|s| !baseline.contains(&s.id)) {
        state().sites.remove(&row.id);
        delete_site(&db, row.id).await.unwrap();
    }
    state().sites.remove(&site_id);
    let rows = db.query("SELECT asteroid_id FROM game.asteroids WHERE system_id=$1 AND field_id=$2", &[&system, &site_id]).await.unwrap();
    for row in rows {
        let id: i64 = row.get("asteroid_id");
        db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&id, &system, &site_id]).await.unwrap();
    }
    db.execute("DELETE FROM game.asteroid_fields WHERE system_id=$1 AND field_id=$2", &[&system, &site_id]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
