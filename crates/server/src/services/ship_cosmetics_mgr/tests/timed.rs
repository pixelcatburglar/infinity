use super::*;


#[test]
fn timed_give_accepts_only_positive_duration_and_false_single_use() {
    let skin = [Value::Int(7)];
    let timed = Value::dict([( "duration", Value::Int(60) )]);
    assert!(matches!(gm_args(&call("GiveSkin", &skin, &timed), 99), Ok((7, GmAction::Give(Some(60))))));
    let permanent = Value::None;
    assert!(matches!(gm_args(&call("GiveSkin", &skin, &permanent), 99), Ok((7, GmAction::Give(None)))));
    let explicit_false = Value::dict([("isSingleUse", Value::Bool(false))]);
    assert!(matches!(gm_args(&call("GiveSkin", &skin, &explicit_false), 99), Ok((7, GmAction::Give(None)))));
    for kwargs in [
        Value::dict([( "duration", Value::Int(0) )]),
        Value::dict([( "duration", Value::Int(-1) )]),
        Value::dict([( "duration", Value::Bool(true) )]),
        Value::dict([( "duration", Value::Float(1.0) )]),
        Value::dict([( "isSingleUse", Value::Int(0) )]),
        Value::dict([( "isSingleUse", Value::Bool(false) ), ("isSingleUse", Value::Bool(false))]),
        Value::dict([( "isSingleUse", Value::Bool(true) )]),
        Value::dict([( "unknown", Value::Int(1) )]),
        Value::dict([( "duration", Value::Int(1) ), ("duration", Value::Int(2))]),
    ] {
        assert!(gm_args(&call("GiveSkin", &skin, &kwargs), 99).is_err());
    }
}

#[test]
fn gm_expire_and_remove_require_exact_shapes_and_character() {
    let skin = [Value::Int(7)];
    let none = Value::None;
    assert!(matches!(gm_args(&call("GMExpireSkinLicense", &skin, &none), 99), Ok((7, GmAction::Expire))));
    let remove = [Value::Int(7), Value::Int(99)];
    assert!(matches!(gm_args(&call("RemoveSkin", &remove, &none), 99), Ok((7, GmAction::Remove))));
    for args in [&skin[..], &remove[..1], &remove[..]] {
        assert!(gm_args(&call("RemoveSkin", args, &none), 100).is_err());
    }
    assert!(gm_args(&call("GMExpireSkinLicense", &remove, &none), 99).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn give_skin_rpc_roles_validation_and_renewal() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let skin = c.sde.ship_skins.licenses().find_map(|license| {
        let skin = c.sde.ship_skins.skin(license.skin_id)?;
        (!skin.is_structure_skin && skin.visible(&c.cfg.server.boot_region)).then_some(skin.skin_id)
    }).expect("visible ordinary SKIN");
    let args = [Value::Int(i64::from(skin))];
    let kwargs = Value::dict([("duration", Value::Int(1))]);
    for role in [0, crate::consts::ROLE_DIRECTOR, crate::consts::ROLE_QA, crate::consts::GM_ROLES & !crate::consts::ROLE_GML] {
        c.login.role = role;
        assert!(dispatch(&mut c, &call("GiveSkin", &args, &kwargs)).await.unwrap().is_err());
        assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_none());
    }
    c.login.role = crate::consts::ROLE_GML;
    for bad in [
        Value::dict([("duration", Value::Bool(true))]),
        Value::dict([("duration", Value::Float(1.0))]),
        Value::dict([("isSingleUse", Value::Int(0))]),
        Value::dict([("isSingleUse", Value::Bool(false)), ("isSingleUse", Value::Bool(false))]),
        Value::dict([("duration", Value::Int(0))]),
        Value::dict([("duration", Value::Int(-1))]),
        Value::dict([("duration", Value::Int(i64::MAX))]),
        Value::dict([("isSingleUse", Value::Bool(true))]),
        Value::dict([("other", Value::Int(1))]),
        Value::dict([("duration", Value::Int(1)), ("duration", Value::Int(2))]),
    ] {
        assert!(dispatch(&mut c, &call("GiveSkin", &args, &bad)).await.unwrap().is_err());
        assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_none());
    }
    assert_eq!(dispatch(&mut c, &call("GiveSkin", &args, &kwargs)).await.unwrap().unwrap(), Value::None);
    let first = ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at.unwrap();
    assert_eq!(dispatch(&mut c, &call("GiveSkin", &args, &kwargs)).await.unwrap().unwrap(), Value::None);
    let second = ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at.unwrap();
    assert_eq!(second, first + crate::consts::FILETIME_DAY);
    assert_eq!(dispatch(&mut c, &call("GiveSkin", &args, &Value::dict([("isSingleUse", Value::Bool(false))]))).await.unwrap().unwrap(), Value::None);
    assert_eq!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at, None);
    assert!(dispatch(&mut c, &call("GiveSkin", &args, &kwargs)).await.unwrap().is_err());
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, fixture.character)]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn timed_item_batch_consumes_each_and_extends_once() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    c.world.register_pilot(fixture.character, tx);
    let station = c.sde.map.stations().next().unwrap().station_id;
    c.session.apply(vec![("stationid", Value::Int(station))]);
    let (item_type, skin, days) = c.sde.ship_skins.licenses().find_map(|license| {
        let skin = c.sde.ship_skins.skin(license.skin_id)?;
        (!license.is_permanent() && !license.is_single_use && !skin.is_structure_skin
            && skin.visible(&c.cfg.server.boot_region) && c.sde.types.get(license.license_type_id).is_some())
            .then_some((license.license_type_id, skin.skin_id, i64::from(license.duration_days)))
    }).expect("visible timed ordinary SKIN item");
    let flag = crate::consts::FLAG_HANGAR;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&item_type, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
        ids.push(item);
    }
    let before = filetime_now();
    assert_eq!(activate(&mut c, ids.clone()).await.unwrap(), Value::None);
    assert_eq!(c.outbox.len(), 2);
    for packet in &c.outbox {
        let crate::macho::Address::Broadcast { broadcast_id, .. } = &packet.destination else { panic!("expected item broadcast") };
        assert_eq!(broadcast_id.as_text().as_deref(), Some("OnItemChange"));
    }
    assert_eq!(rx.try_recv().unwrap(), ("OnSkinLicenseActivated".to_string(), vec![Value::Int(i64::from(skin)), Value::Int(fixture.character)]));
    assert!(rx.try_recv().is_err());
    let after = filetime_now();
    let expiry = ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at.unwrap();
    let duration = 2 * days * crate::consts::FILETIME_DAY;
    assert!(expiry >= before + duration && expiry <= after + duration);
    for id in ids { assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(id)).await.unwrap()[0].quantity, 1); }
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, fixture.character)]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn expired_application_is_not_resurrected_by_give() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station = c.sde.map.stations().next().unwrap().station_id;
    c.session.apply(vec![("stationid", Value::Int(station))]);
    c.login.role = crate::consts::ROLE_GML;
    let (skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
        let skin = c.sde.ship_skins.skin(license.skin_id)?;
        if skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) { return None; }
        let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)))?;
        Some((skin.skin_id, hull))
    }).expect("visible ordinary ship SKIN");
    let flag = crate::consts::FLAG_HANGAR;
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    let args = [Value::Int(i64::from(skin))];
    let one_day = Value::dict([("duration", Value::Int(1))]);
    assert_eq!(dispatch(&mut c, &call("GiveSkin", &args, &one_day)).await.unwrap().unwrap(), Value::None);
    assert_eq!(apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
    let expiry = ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at;
    assert!(ship_skins::update_expiry(&db, fixture.character, skin, expiry, Some(filetime_now() - 1)).await.unwrap());
    assert_eq!(dispatch(&mut c, &call("GiveSkin", &args, &one_day)).await.unwrap().unwrap(), Value::None);
    assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_some());
    assert_eq!(ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, None);
    assert!(ship_skins::applications_for_character(&db, fixture.character).await.unwrap().is_empty());
    assert_eq!(apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
    let expiry = ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at;
    assert!(ship_skins::update_expiry(&db, fixture.character, skin, expiry, Some(filetime_now() - 1)).await.unwrap());
    assert_eq!(dispatch(&mut c, &call("GiveSkin", &args, &Value::None)).await.unwrap().unwrap(), Value::None);
    assert_eq!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at, None);
    assert!(ship_skins::applications_for_character(&db, fixture.character).await.unwrap().is_empty());
    assert_eq!(ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, None);
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, fixture.character)]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn expired_application_batch_rollback_and_no_resurrection() {
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
        let mut stream = crate::gateway::tests::notice_stream(&c.world, [2; 16], fixture.character, 16);
        let station = c.sde.map.stations().next().unwrap().station_id;
        c.session.apply(vec![("stationid", Value::Int(station))]);
        let flag = crate::consts::FLAG_HANGAR;
        for permanent in [false, true] {
            let (item_type, skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
                let skin = c.sde.ship_skins.skin(license.skin_id)?;
                if license.is_permanent() != permanent || license.is_single_use || skin.is_structure_skin
                    || !skin.visible(&c.cfg.server.boot_region) || c.sde.types.get(license.license_type_id).is_none() {
                    return None;
                }
                let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)))?;
                Some((license.license_type_id, skin.skin_id, hull))
            }).expect("visible SKIN item with ship hull");
            let ship: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
            let good: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&item_type, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
            let bad: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
            assert!(ship_skins::insert_license(&test_db, fixture.character, skin, Some(filetime_now() + crate::consts::FILETIME_DAY)).await.unwrap());
            assert_eq!(apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
            while stream.rx.try_recv().is_ok() {}
            let expiry = ship_skins::get_stored_license(&test_db, fixture.character, skin).await.unwrap().unwrap().expires_at;
            let past = Some(filetime_now() - 1);
            assert!(ship_skins::update_expiry(&test_db, fixture.character, skin, expiry, past).await.unwrap());
            c.outbox.clear();
            let invalid = [Value::list([Value::Int(good), Value::Int(bad)])];
            assert!(dispatch(&mut c, &call("ActivateSkinLicense", &invalid, &Value::None)).await.unwrap().is_err());
            assert_eq!(ship_skins::get_stored_license(&test_db, fixture.character, skin).await.unwrap().unwrap().expires_at, past);
            assert_eq!(ship_skins::applications_for_character(&test_db, fixture.character).await.unwrap().len(), 1);
            assert_eq!(load_items(&test_db, &c.sde, ItemQuery::Id(good)).await.unwrap()[0].quantity, 2);
            assert_eq!(load_items(&test_db, &c.sde, ItemQuery::Id(bad)).await.unwrap()[0].quantity, 1);
            assert!(rx.try_recv().is_err());
            assert!(stream.rx.try_recv().is_err());
            assert!(c.outbox.is_empty());
            let valid = [Value::list([Value::Int(good)])];
            assert_eq!(dispatch(&mut c, &call("ActivateSkinLicense", &valid, &Value::None)).await.unwrap().unwrap(), Value::None);
            let license = ship_skins::get_stored_license(&test_db, fixture.character, skin).await.unwrap().unwrap();
            assert_eq!(license.expires_at.is_none(), permanent);
            assert!(ship_skins::applications_for_character(&test_db, fixture.character).await.unwrap().is_empty());
            assert_eq!(ship_skins::states_for_ships(&test_db, &[ship], filetime_now()).await.unwrap()[0].skin_id, None);
            assert_eq!(load_items(&test_db, &c.sde, ItemQuery::Id(good)).await.unwrap()[0].quantity, 1);
            assert_eq!(rx.try_recv().unwrap(), ("OnSkinLicenseRemoved".to_string(), vec![Value::Int(i64::from(skin)), Value::Int(ship), Value::Int(i64::from(hull))]));
            assert_eq!(rx.try_recv().unwrap(), ("OnSkinLicenseActivated".to_string(), vec![Value::Int(i64::from(skin)), Value::Int(fixture.character)]));
            assert!(rx.try_recv().is_err());
            let (notice, payload) = crate::gateway::tests::decode_notice(&stream.rx.try_recv().unwrap());
            assert_eq!(notice.target_group.unwrap().target, Some(crate::gateway::protocol::target_group::Target::Character(u32::try_from(fixture.character).unwrap())));
            let state = payload.get_field_by_name("state").unwrap().as_message().unwrap().clone();
            assert_eq!(state.get_field_by_name("ship").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u64(), Some(ship as u64));
            assert_eq!(state.get_field_by_name("skin").unwrap().as_message().unwrap().get_field_by_name("no_skin").unwrap().as_bool(), Some(true));
            assert!(stream.rx.try_recv().is_err());
            test_db.execute("DELETE FROM game.ship_skin_licenses WHERE character_id=$1 AND skin_id=$2", &[&fixture.character, &skin]).await.unwrap();
        }
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", expiry::KIND, fixture.character)]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
