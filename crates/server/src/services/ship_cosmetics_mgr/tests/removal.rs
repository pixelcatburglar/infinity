use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn empty_removal_and_repeat_are_authoritative() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        c.world.register_pilot(fixture.character, tx);
        c.session.apply(vec![("userid", Value::Int(fixture.account))]);
        c.world.set_online_session(fixture.character, crate::session::OnlineSession::from_session(&c.session));
        let mut stream = crate::gateway::tests::notice_stream(&c.world, [1; 16], fixture.character, 8);
        let skin = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            (!skin.is_structure_skin && skin.visible(&c.cfg.server.boot_region)).then_some(skin.skin_id)
        }).expect("visible ordinary SKIN");
        c.login.role = crate::consts::ROLE_GML;
        assert!(ship_skins::insert_license(&test_db, fixture.character, skin, None).await.unwrap());
        let args = [Value::Int(i64::from(skin)), Value::Int(fixture.character)];
        let kwargs = Value::None;
        assert_eq!(dispatch(&mut c, &call("RemoveSkin", &args, &kwargs)).await.unwrap().unwrap(), Value::None);
        assert!(ship_skins::get_stored_license(&test_db, fixture.character, skin).await.unwrap().is_none());
        assert_eq!(rx.try_recv().unwrap(), ("OnRemoteMessage".to_string(), vec![
            Value::str(crate::messages::MSG_CUSTOM_NOTIFY),
            Value::dict([("notify", Value::unicode(crate::messages::ship_skins::REMOVED_NEXT_LOGIN))]),
        ]));
        assert!(rx.try_recv().is_err());
        assert!(stream.rx.try_recv().is_err());
        assert!(dispatch(&mut c, &call("RemoveSkin", &args, &kwargs)).await.unwrap().is_err());
        assert!(rx.try_recv().is_err());
        assert!(stream.rx.try_recv().is_err());
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, fixture.character)]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn docked_multi_hull_removal_and_clear_are_idempotent() {
    use prost_reflect::ReflectMessage;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), fixture.character).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        c.world.register_pilot(fixture.character, tx);
        c.session.apply(vec![("userid", Value::Int(fixture.account))]);
        c.world.set_online_session(fixture.character, crate::session::OnlineSession::from_session(&c.session));
        let mut stream = crate::gateway::tests::notice_stream(&c.world, [3; 16], fixture.character, 16);
        let station = c.sde.map.stations().next().unwrap().station_id;
        c.session.apply(vec![("stationid", Value::Int(station))]);
        c.login.role = crate::consts::ROLE_GML;
        let (skin, hulls) = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            if skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) { return None; }
            let mut hulls: Vec<_> = skin.type_ids.iter().copied().filter(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP))).collect();
            hulls.sort_unstable(); hulls.dedup();
            (hulls.len() >= 2).then(|| (skin.skin_id, [hulls[0], hulls[1]]))
        }).expect("visible SKIN supporting two ship hulls");
        let flag = crate::consts::FLAG_HANGAR;
        let mut ships = Vec::new();
        for hull in hulls {
            let id: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
            ships.push((id, hull));
        }
        for method in ["RemoveSkin", "GMExpireSkinLicense"] {
            assert!(ship_skins::insert_license(&test_db, fixture.character, skin, None).await.unwrap());
            for (ship, _) in &ships { assert_eq!(apply(&mut c, *ship, Some(skin)).await.unwrap(), Value::None); }
            while stream.rx.try_recv().is_ok() {}
            let args = if method == "RemoveSkin" { vec![Value::Int(i64::from(skin)), Value::Int(fixture.character)] } else { vec![Value::Int(i64::from(skin))] };
            assert_eq!(dispatch(&mut c, &call(method, &args, &Value::None)).await.unwrap().unwrap(), Value::None);
            let mut removed = Vec::new();
            for _ in &ships {
                let (event, args) = rx.try_recv().unwrap();
                assert_eq!(event, "OnSkinLicenseRemoved");
                removed.push(args);
                let bytes = stream.rx.try_recv().unwrap();
                let (notice, payload) = crate::gateway::tests::decode_notice(&bytes);
                assert_eq!(notice.target_group.unwrap().target, Some(crate::gateway::protocol::target_group::Target::Character(u32::try_from(fixture.character).unwrap())));
                assert_eq!(payload.descriptor().full_name(), "eve_public.cosmetic.ship.api.SetNotice");
                let state = payload.get_field_by_name("state").unwrap().as_message().unwrap().clone();
                let ship = state.get_field_by_name("ship").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u64().unwrap() as i64;
                assert!(ships.iter().any(|(id, _)| *id == ship));
                assert_eq!(state.get_field_by_name("character").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u32(), Some(0));
                assert_eq!(state.get_field_by_name("skin").unwrap().as_message().unwrap().get_field_by_name("no_skin").unwrap().as_bool(), Some(true));
            }
            for (ship, hull) in &ships {
                assert!(removed.contains(&vec![Value::Int(i64::from(skin)), Value::Int(*ship), Value::Int(i64::from(*hull))]));
                assert_eq!(apply(&mut c, *ship, None).await.unwrap(), Value::None);
            }
            assert!(rx.try_recv().is_err());
            assert!(stream.rx.try_recv().is_err());
            assert!(dispatch(&mut c, &call(method, &args, &Value::None)).await.unwrap().is_err());
            expiry::converge(&test_db, &c.world, fixture.character, filetime_now()).await.unwrap();
            assert!(rx.try_recv().is_err());
            assert!(stream.rx.try_recv().is_err());
        }
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, fixture.character)]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn active_removal_reaches_visible_observer_with_pilot_state() {
    use prost_reflect::ReflectMessage;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let observer = crate::services::tests::fixture::create(&db).await.unwrap();
    let outsider = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let station_row = c.sde.map.stations().find(|s| s.solar_system_id.is_some()).unwrap();
        let station = station_row.station_id;
        let system = station_row.solar_system_id.unwrap();
        c.session.apply(vec![("stationid", Value::Int(station)), ("userid", Value::Int(owner.account))]);
        c.login.role = crate::consts::ROLE_GML;
        let (skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
            let skin = c.sde.ship_skins.skin(license.skin_id)?;
            if skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) { return None; }
            let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)))?;
            Some((skin.skin_id, hull))
        }).expect("visible ship SKIN");
        let flag = crate::consts::FLAG_HANGAR;
        let mut ships = Vec::new();
        for f in [owner, observer, outsider] {
            let ship: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &f.character, &station, &flag]).await.unwrap().get("item_id");
            ships.push(ship);
        }
        let mut receivers = Vec::new();
        for (f, ship) in [(owner, ships[0]), (observer, ships[1]), (outsider, ships[2])] {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            c.world.register_pilot(f.character, tx);
            let mut session = crate::session::OnlineSession::from_session(&c.session).unwrap();
            session.character_id = f.character; session.user_id = Some(f.account);
            session.ship_id = Some(ship); session.station_id = None; session.system_id = Some(system);
            c.world.set_online_session(f.character, Some(session));
            receivers.push(rx);
        }
        let mut owner_stream = crate::gateway::tests::notice_stream(&c.world, [4; 16], owner.character, 32);
        let mut observer_stream = crate::gateway::tests::notice_stream(&c.world, [5; 16], observer.character, 32);
        let mut outsider_stream = crate::gateway::tests::notice_stream(&c.world, [6; 16], outsider.character, 32);
        for (index, f) in [owner, observer, outsider].into_iter().enumerate() {
            let free = crate::dao::items::space_ships::FreeShip { item_id: ships[index], type_id: hull, owner_id: f.character, corporation_id: 1, alliance_id: -1, pos: [index as f64 * 1000.0, 0.0, 0.0] };
            let ball = crate::services::ship::balls::piloted(&test_db, c.sde.clone(), &free, f.character, 0.0).await.unwrap().install();
            c.world.add_ship(system, ball).await.unwrap();
        }
        let (tx, _destiny) = tokio::sync::mpsc::unbounded_channel();
        c.world.observe(system, owner.character, ships[0], tx).await.unwrap();
        let (tx, _destiny_observer) = tokio::sync::mpsc::unbounded_channel();
        c.world.observe(system, observer.character, ships[1], tx).await.unwrap();
        let timeout = std::time::Duration::from_secs(15);
        tokio::time::timeout(timeout, owner_stream.rx.recv()).await.unwrap().unwrap();
        tokio::time::timeout(timeout, observer_stream.rx.recv()).await.unwrap().unwrap();
        for method in ["RemoveSkin", "GMExpireSkinLicense"] {
            assert!(ship_skins::insert_license(&test_db, owner.character, skin, None).await.unwrap());
            assert_eq!(apply(&mut c, ships[0], Some(skin)).await.unwrap(), Value::None);
            while owner_stream.rx.try_recv().is_ok() {}
            while observer_stream.rx.try_recv().is_ok() {}
            let args = if method == "RemoveSkin" { vec![Value::Int(i64::from(skin)), Value::Int(owner.character)] } else { vec![Value::Int(i64::from(skin))] };
            assert_eq!(dispatch(&mut c, &call(method, &args, &Value::None)).await.unwrap().unwrap(), Value::None);
            assert_eq!(receivers[0].try_recv().unwrap(), ("OnSkinLicenseRemoved".to_string(), vec![Value::Int(i64::from(skin)), Value::Int(ships[0]), Value::Int(i64::from(hull))]));
            for (stream, target) in [(&mut owner_stream, owner.character), (&mut observer_stream, observer.character)] {
                let bytes = tokio::time::timeout(timeout, stream.rx.recv()).await.unwrap().unwrap();
                let (notice, payload) = crate::gateway::tests::decode_notice(&bytes);
                assert_eq!(notice.target_group.unwrap().target, Some(crate::gateway::protocol::target_group::Target::Character(u32::try_from(target).unwrap())));
                assert_eq!(payload.descriptor().full_name(), "eve_public.cosmetic.ship.api.SetNotice");
                let state = payload.get_field_by_name("state").unwrap().as_message().unwrap().clone();
                assert_eq!(state.get_field_by_name("ship").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u64(), Some(ships[0] as u64));
                assert_eq!(state.get_field_by_name("character").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u32(), Some(owner.character as u32));
                assert_eq!(state.get_field_by_name("skin").unwrap().as_message().unwrap().get_field_by_name("no_skin").unwrap().as_bool(), Some(true));
                assert!(stream.rx.try_recv().is_err());
            }
            assert!(outsider_stream.rx.try_recv().is_err());
            assert!(receivers[0].try_recv().is_err());
        }
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, owner.character)]).await.unwrap();
    for fixture in [owner, observer, outsider] { crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap(); }
    result.unwrap();
}
