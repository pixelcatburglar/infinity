//! Argument and wire-shape coverage for first-party SKIN calls.
use super::*;

#[test]
fn strict_activation_arguments() {
    assert_eq!(activation_ids(&[Value::list([Value::Int(2), Value::Int(1)])]).unwrap(), vec![1, 2]);
    for args in [vec![], vec![Value::list([])], vec![Value::list([Value::Int(1), Value::Int(1)])], vec![Value::list([Value::Bool(true)])], vec![Value::list([Value::Int(0)])], vec![Value::Tuple(vec![Value::Int(1)])]] {
        assert!(activation_ids(&args).is_err());
    }
    assert!(small(&Value::Bool(true)).is_err());
    assert!(small(&Value::Int(i64::MAX)).is_err());
}

#[test]
fn license_record_is_keyval_with_nullable_expiry() {
    let value = record(ship_skins::License { skin_id: 1, expires_at: None }, true);
    assert_eq!(value, keyval([("expires", Value::None), ("isSingleUse", Value::Bool(false)), ("skinID", Value::Int(1))]));
    let metadata = record(ship_skins::License { skin_id: 1, expires_at: Some(2) }, false);
    assert_eq!(metadata, keyval([("expires", Value::Int(2)), ("isSingleUse", Value::Bool(false))]));
    for value in [value, metadata, Value::None, Value::list([])] {
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn permanent_activation_and_application() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let station = c.sde.map.stations().next().unwrap().station_id;
    c.session.apply(vec![("stationid", Value::Int(station))]);
    let (license_type, skin, hull, second_hull) = c.sde.ship_skins.licenses().find_map(|license| {
        let skin = c.sde.ship_skins.skin(license.skin_id)?;
        if !license.is_permanent() || license.is_single_use || skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) || c.sde.types.get(license.license_type_id).is_none() { return None; }
        let hull = *skin.type_ids.first()?;
        let second_hull = *skin.type_ids.get(1)?;
        if hull == second_hull || ![hull, second_hull].iter().all(|id| c.sde.types.get(*id).is_some_and(|ty| ty.category_id == Some(crate::consts::CATEGORY_SHIP))) { return None; }
        Some((license.license_type_id, skin.skin_id, hull, second_hull))
    }).expect("visible permanent multi-hull SKIN with two ship hulls and license type");
    let flag = crate::consts::FLAG_HANGAR;
    let first: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,2,false) RETURNING item_id", &[&license_type, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    let second: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&license_type, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    let docked = Identity::of_client(&c);
    let projected = state::get(&db, &c.world, &docked, ship, filetime_now()).await.unwrap().unwrap();
    assert_eq!(projected.skin_id, None);
    assert_eq!(projected.ship_id, ship);
    assert!(state::get(&db, &c.world, &docked, i64::MAX, filetime_now()).await.unwrap().is_none());
    assert!(state::all(&db, &c.world, &c.sde, &docked, filetime_now()).await.unwrap().is_empty());
    let kwargs = Value::Dict(vec![]);
    for (args, expected) in [
        (vec![Value::Int(first)], text::STRUCTURE),
        (vec![], text::INVALID_ARGUMENT),
        (vec![Value::Bool(true)], text::INVALID_ARGUMENT),
        (vec![Value::Int(first), Value::Int(second)], text::INVALID_ARGUMENT),
    ] {
        let call = Call { service: "shipCosmeticsMgr", method: "ActivateStructureSkinR", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let result = dispatch(&mut c, &call).await.unwrap();
        assert!(matches!(result, Err(CallError::User { dict, .. }) if dict.iter().any(|(_, value)| value == &Value::unicode(expected))));
        assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(first)).await.unwrap()[0].quantity, 2);
        assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_none());
    }
    assert!(activate(&mut c, vec![first, second]).await.is_err());
    assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_none());
    assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(first)).await.unwrap()[0].quantity, 2);
    assert!(activate(&mut c, vec![first, ship]).await.is_err());
    assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_none());
    assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(first)).await.unwrap()[0].quantity, 2);
    let other_owner = crate::services::tests::fixture::create(&db).await.unwrap();
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&second, &other_owner.character]).await.unwrap();
    assert!(activate(&mut c, vec![second]).await.is_err());
    db.execute("UPDATE game.items SET owner_id=$2 WHERE item_id=$1", &[&second, &fixture.character]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, other_owner).await.unwrap();
    let identity = Identity::of_client(&c);
    let endpoint = Endpoint { location: station, flag };
    let reach = Reachability::capture(&identity, &db, &c.world, None, &[endpoint]).await.unwrap();
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&first, &fixture.character]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    assert!(licenses::activate_tx(&db, &identity, &c.sde, &c.cfg.server.boot_region, &reach, &[first], &mut outbox).await.is_err());
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&first, &station]).await.unwrap();
    assert!(ship_skins::insert_license(&db, fixture.character, skin, Some(filetime_now() - 1)).await.unwrap());
    assert_eq!(activate(&mut c, vec![first]).await.unwrap(), Value::None);
    assert_eq!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().unwrap().expires_at, None);
    assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(first)).await.unwrap()[0].quantity, 1);
    let collection = query(&c, "GetLicencedSkins", &[]).await.unwrap();
    assert_eq!(collection.items().unwrap().len(), 1);
    let filtered = query(&c, "GetLicencedSkinsForShipType", &[Value::Int(fixture.character), Value::Int(i64::from(hull))]).await.unwrap();
    assert_eq!(filtered, collection);
    let filtered_second = query(&c, "GetLicencedSkinsForShipType", &[Value::Int(fixture.character), Value::Int(i64::from(second_hull))]).await.unwrap();
    assert_eq!(filtered_second, collection);
    let second_ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&second_hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    assert_eq!(apply(&mut c, second_ship, Some(skin)).await.unwrap(), Value::None);
    assert_eq!(state::get(&db, &c.world, &docked, second_ship, filetime_now()).await.unwrap().unwrap().skin_id, Some(skin));
    let other = crate::services::tests::fixture::create(&db).await.unwrap();
    assert!(query(&c, "GetLicencedSkinsForShipType", &[Value::Int(other.character), Value::Int(i64::from(hull))]).await.is_err());
    assert_eq!(query(&c, "GetFirstPartySkinData", &[Value::Int(other.character), Value::Int(i64::from(skin))]).await.unwrap(), Value::None);
    assert!(ship_skins::insert_license(&db, other.character, skin, None).await.unwrap());
    assert_ne!(query(&c, "GetFirstPartySkinData", &[Value::Int(other.character), Value::Int(i64::from(skin))]).await.unwrap(), Value::None);
    crate::services::tests::fixture::cleanup(&db, other).await.unwrap();
    assert!(activate(&mut c, vec![second]).await.is_err());
    assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(second)).await.unwrap()[0].quantity, 1);
    let incompatible = c.sde.types.iter().find(|ty| ty.category_id == Some(crate::consts::CATEGORY_SHIP) && !c.sde.ship_skins.skin(skin).unwrap().supports(ty.type_id)).unwrap().type_id;
    let wrong_ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&incompatible, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    assert!(apply(&mut c, wrong_ship, Some(skin)).await.is_err());
    let identity = Arc::new(Identity::of_client(&c));
    let endpoint = Endpoint { location: ship, flag };
    let reach = Reachability::capture(&identity, &db, &c.world, None, &[endpoint]).await.unwrap();
    use crate::gateway::protocol::target_group::Target;
    let app = [19; 16];
    let mut registration = c.world.notice_hub().register(app, 4);
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for mode in 0..3 {
        let identity = identity.clone(); let reach = reach.clone(); let sde = c.sde.clone();
        let region = c.cfg.server.boot_region.clone(); let attempts = attempts.clone();
        let world = c.world.clone();
        let result: Result<(), CallError> = db.transaction(move |tx, outbox: &mut Outbox| {
            let identity = identity.clone(); let reach = reach.clone(); let sde = sde.clone(); let region = region.clone(); let attempts = attempts.clone();
            Box::pin(async move {
                let now = filetime_now();
                let requested = if mode == 1 { None } else { Some(skin) };
                let changed = application::apply_tx(tx, &identity, &sde, &region, &reach, ship, requested, now).await?;
                assert!(changed.is_some());
                let facts = [crate::space::ship_cosmetics::Ship { ship_id: ship, type_id: hull, character_id: Some(identity.char_id), is_npc: false }];
                let states = state::project(tx, &sde, &facts, now).await?;
                let notice = crate::gateway::ship_cosmetics::notice(&states[0], &[identity.char_id])?.remove(0);
                outbox.notify_gateway(notice.retarget(Target::ApplicationInstanceUuid(app.to_vec())).map_err(crate::gateway::notices::producer_error)?);
                if mode == 1 { return Err(CallError::Internal(anyhow::anyhow!("rollback"))); }
                if mode == 2 && attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    tx.batch_execute("DO $$ BEGIN RAISE EXCEPTION 'retry' USING ERRCODE = '40001'; END $$").await.map_err(crate::db::Error::from)?;
                }
                Ok(())
            })
        }, move |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &world) }).await;
        assert_eq!(result.is_err(), mode == 1);
        let stored = ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap();
        assert_eq!(stored[0].skin_id, Some(skin));
        if mode == 1 {
            assert!(registration.rx.try_recv().is_err());
            assert_eq!(apply(&mut c, ship, None).await.unwrap(), Value::None);
        } else {
            let frame = registration.rx.try_recv().unwrap();
            let (envelope, decoded) = crate::gateway::tests::decode_notice(&frame);
            assert_eq!(envelope.target_group.unwrap().target, Some(Target::ApplicationInstanceUuid(app.to_vec())));
            assert!(envelope.payload.unwrap().type_url.ends_with("eve_public.cosmetic.ship.api.SetNotice"));
            let state = decoded.get_field_by_name("state").unwrap().as_message().unwrap().clone();
            let encoded_ship = state.get_field_by_name("ship").unwrap().as_message().unwrap().clone();
            assert_eq!(encoded_ship.get_field_by_name("sequential").unwrap().as_u64(), Some(u64::try_from(ship).unwrap()));
            let encoded_skin = state.get_field_by_name("skin").unwrap().as_message().unwrap().clone();
            let first = encoded_skin.get_field_by_name("firstparty").unwrap().as_message().unwrap().clone();
            let id = first.get_field_by_name("identifier").unwrap().as_message().unwrap().clone();
            assert_eq!(id.get_field_by_name("sequential").unwrap().as_u64(), Some(u64::try_from(skin).unwrap()));
            assert!(registration.rx.try_recv().is_err());
        }
    }
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    let make_notice = || crate::gateway::ship_cosmetics::notice(&state::State { ship_id: ship, character_id: fixture.character, skin_id: Some(skin) }, &[fixture.character]).unwrap().remove(0).retarget(Target::ApplicationInstanceUuid(app.to_vec())).unwrap();
    let mut outbox: Outbox = Outbox::default();
    outbox.notify_gateway(make_notice());
    crate::db::outbox::dispatch_world((), outbox, &c.world).unwrap();
    assert!(registration.rx.try_recv().is_ok());
    let mut outbox: Outbox = Outbox::default();
    outbox.notify_gateway(make_notice());
    crate::db::outbox::dispatch_client((), outbox, &mut c).unwrap();
    assert!(registration.rx.try_recv().is_ok());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    crate::db::outbox::Notification::Gateway(make_notice()).dispatch_sender(&c.world, &sender).unwrap();
    assert!(registration.rx.try_recv().is_ok());
    let mut custom = Outbox::<crate::services::effects::online::OnlineChanged>::default();
    custom.notify_gateway(make_notice());
    crate::services::effects::online::dispatch_changed((), custom, &c).unwrap();
    assert!(registration.rx.try_recv().is_ok());
    assert_eq!(apply(&mut c, ship, None).await.unwrap(), Value::None);
    assert_eq!(apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
    assert!(matches!(apply(&mut c, ship, Some(skin)).await, Err(CallError::User { msg, .. }) if msg == crate::messages::MSG_SKIN_ALREADY_APPLIED));
    assert_eq!(apply(&mut c, ship, None).await.unwrap(), Value::None);
    assert_eq!(apply(&mut c, ship, None).await.unwrap(), Value::None);
    assert_eq!(apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
    let partner = crate::services::tests::fixture::create(&db).await.unwrap();
    let trade = crate::dao::trades::create(&db, station, fixture.character, partner.character, filetime_now()).await.unwrap();
    let ancestor: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &trade.id, &flag]).await.unwrap().get("item_id");
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=ANY($1)", &[&vec![first, ship], &ancestor]).await.unwrap();
    assert_eq!(crate::dao::trades::reserved(&db, &[first, ship]).await.unwrap(), vec![first.min(ship), first.max(ship)]);
    assert!(activate(&mut c, vec![first]).await.is_err());
    assert!(apply(&mut c, ship, None).await.is_err());
    assert!(apply(&mut c, ship, Some(skin)).await.is_err());
    assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(first)).await.unwrap()[0].quantity, 1);
    assert!(ship_skins::get_stored_license(&db, fixture.character, skin).await.unwrap().is_some());
    assert_eq!(ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, Some(skin));
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade.id]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, partner).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE and external flock"]
async fn corporation_take_controls_personal_license_and_ship() {
    use crate::services::tests::fixture;
    use crate::consts::{FLAG_CORP_SAG1, FLAG_HANGAR, ROLE_HANGAR_CAN_TAKE_1};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let actor = fixture::create(&db).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let (mut c, _peer) = crate::services::tests::client(db.clone(), actor.character).await;
    let station = c.sde.map.stations().next().unwrap().station_id;
    let (license_type, skin, hull) = c.sde.ship_skins.licenses().find_map(|license| {
        let skin = c.sde.ship_skins.skin(license.skin_id)?;
        if !license.is_permanent() || license.is_single_use || skin.is_structure_skin || !skin.visible(&c.cfg.server.boot_region) || c.sde.types.get(license.license_type_id).is_none() { return None; }
        let hull = skin.type_ids.iter().copied().find(|id| c.sde.types.get(*id).is_some_and(|ty| ty.category_id == Some(crate::consts::CATEGORY_SHIP)))?;
        Some((license.license_type_id, skin.skin_id, hull))
    }).expect("visible permanent SKIN with ship hull");
    let corp = fixture::create_corporation(&db, ceo.character, station).await.unwrap();
    fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    c.session.apply(vec![("stationid", Value::Int(station)), ("corpid", Value::Int(corp))]);
    let office: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&license_type, &corp, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
    fixture::create_office(&db, office, corp, station).await.unwrap();
    let license: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&license_type, &actor.character, &office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &actor.character, &office, &FLAG_CORP_SAG1]).await.unwrap().get("item_id");
    assert!(activate(&mut c, vec![license]).await.is_err());
    assert_eq!(load_items(&db, &c.sde, ItemQuery::Id(license)).await.unwrap()[0].quantity, 1);
    assert!(ship_skins::get_stored_license(&db, actor.character, skin).await.unwrap().is_none());
    fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    assert_eq!(activate(&mut c, vec![license]).await.unwrap(), Value::None);
    fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    assert!(apply(&mut c, ship, Some(skin)).await.is_err());
    assert_eq!(ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, None);
    fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    assert_eq!(apply(&mut c, ship, Some(skin)).await.unwrap(), Value::None);
    fixture::assign_corporation(&db, actor.character, corp, 0).await.unwrap();
    assert!(apply(&mut c, ship, None).await.is_err());
    assert_eq!(ship_skins::states_for_ships(&db, &[ship], filetime_now()).await.unwrap()[0].skin_id, Some(skin));
    fixture::assign_corporation(&db, actor.character, corp, ROLE_HANGAR_CAN_TAKE_1).await.unwrap();
    assert_eq!(apply(&mut c, ship, None).await.unwrap(), Value::None);
    fixture::cleanup_corporation(&db, corp, &[actor.character], office).await.unwrap();
    fixture::cleanup(&db, actor).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
}