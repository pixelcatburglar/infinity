use super::*;

#[test]
fn state_encodes_checked_identifiers_and_skin_oneof() {
    let state = State { ship_id: i64::from(u32::MAX) + 1, character_id: i64::from(u32::MAX), skin_id: Some(1) };
    let encoded = encode(&state).unwrap();
    assert_eq!(msg_field(&encoded, "ship").unwrap().get_field_by_name("sequential").unwrap().as_u64(), Some(state.ship_id as u64));
    assert_eq!(msg_field(&encoded, "character").unwrap().get_field_by_name("sequential").unwrap().as_u32(), Some(u32::MAX));
    let skin = msg_field(&encoded, "skin").unwrap();
    assert!(skin.has_field_by_name("firstparty"));
    assert!(!skin.has_field_by_name("no_skin"));
    assert_eq!(msg_field(&msg_field(&skin, "firstparty").unwrap(), "identifier").unwrap().get_field_by_name("sequential").unwrap().as_u64(), Some(1));
    let clear = encode(&State { skin_id: None, ..state.clone() }).unwrap();
    let skin = msg_field(&clear, "skin").unwrap();
    assert_eq!(skin.get_field_by_name("no_skin").unwrap().as_bool(), Some(true));
    assert!(!skin.has_field_by_name("firstparty"));
    assert!(encode(&State { character_id: i64::from(u32::MAX) + 1, ..state.clone() }).is_err());
    assert!(encode(&State { ship_id: -1, ..state }).is_err());
}

#[test]
fn notice_shapes_and_recipient_deduplication() {
    validate().unwrap();
    let state = State { ship_id: 1, character_id: 0, skin_id: None };
    assert_eq!(notice(&state, &[2, 2, 0, -1, 3]).unwrap().len(), 2);
    assert!(notice(&state, &[]).unwrap().is_empty());
    assert!(all_notice(&[], 1).is_ok());
    let overflow = i64::from(u32::MAX) + 1;
    assert!(notice(&state, &[2, overflow]).is_err());
    assert!(notice(&state, &[overflow]).is_err());
    assert!(all_notice(&[state], overflow).is_err());
    let all = message(SET_ALL).unwrap();
    assert!(all.get_field_by_name("state").unwrap().is_list());
    assert!(all.get_field_by_name("states").is_none());
    let response = response_for(ALL).unwrap();
    assert!(response.descriptor().get_field_by_name("states").unwrap().is_list());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE"]
async fn query_status_distinguishes_unavailable_actor_and_empty_scope() {
    use prost::Message;
    use crate::gateway::{protocol::{AuthoritativeContext, Identifier32, Request, UserIdentifier}, requests::handle_request, Gateway};
    use infinity_core::session::OnlineSession;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
    let gateway = Gateway::new(db.clone(), client.world.clone(), client.cfg.clone());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    gateway.world.register_pilot(fixture.character, tx);
    let mut session = OnlineSession { character_id: fixture.character, user_id: Some(fixture.account), session_id: 1, ship_id: None, station_id: None, system_id: None, corporation_id: None, alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0 };
    let make_request = |name: &str| {
        let mut payload = DynamicMessage::new(message(name).unwrap());
        if name == GET {
            let ship = identifier(&payload.descriptor(), "ship", PV::U64(1)).unwrap();
            set(&mut payload, "ship", PV::Message(ship)).unwrap();
        }
        Request { issued: None, correlation_uuid: vec![1; 16], external_origin: String::new(), application_instance_uuid: vec![2; 16],
            authoritative_context: Some(AuthoritativeContext { tenant: String::new(), authenticated_user: Some(UserIdentifier { sequential: i32::try_from(fixture.account).unwrap() }), active_character: Some(Identifier32 { sequential: u32::try_from(fixture.character).unwrap() }), identity: None }),
            payload: Some(prost_types::Any { type_url: format!("type.googleapis.com/{name}"), value: payload.encode_to_vec() }) }
    };
    gateway.world.set_online_session(fixture.character, Some(session.clone()));
    for name in [GET, ALL] {
        let response = handle_request(&gateway, make_request(name), None).await;
        assert_eq!(response.status_code, 500);
        assert!(response.payload.is_none());
    }
    session.system_id = Some(i64::MAX);
    gateway.world.set_online_session(fixture.character, Some(session.clone()));
    for name in [GET, ALL] {
        let response = handle_request(&gateway, make_request(name), None).await;
        assert_eq!(response.status_code, 500);
        assert!(response.payload.is_none());
    }
    session.station_id = Some(client.sde.map.stations().next().unwrap().station_id);
    gateway.world.set_online_session(fixture.character, Some(session.clone()));
    let response = handle_request(&gateway, make_request(ALL), None).await;
    assert_eq!(response.status_code, 200);
    let payload = response.payload.unwrap();
    let decoded = DynamicMessage::decode(response_for(ALL).unwrap().descriptor(), payload.value.as_slice()).unwrap();
    assert!(decoded.get_field_by_name("states").unwrap().as_list().unwrap().is_empty());
    let (skin, hull) = client.sde.ship_skins.licenses().find_map(|license| {
        let skin = client.sde.ship_skins.skin(license.skin_id)?;
        if !license.is_permanent() || license.is_single_use || skin.is_structure_skin || !skin.visible(&client.cfg.server.boot_region) { return None; }
        let hull = skin.type_ids.iter().copied().find(|id| client.sde.types.get(*id).is_some_and(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP)))?;
        Some((skin.skin_id, hull))
    }).expect("visible permanent SKIN with ship hull");
    let station = session.station_id.unwrap();
    let flag = infinity_core::consts::FLAG_HANGAR;
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &fixture.character, &station, &flag]).await.unwrap().get("item_id");
    let mut request = make_request(GET);
    let mut payload = DynamicMessage::new(message(GET).unwrap());
    let id = identifier(&payload.descriptor(), "ship", PV::U64(u64::try_from(ship).unwrap())).unwrap();
    set(&mut payload, "ship", PV::Message(id)).unwrap();
    request.payload.as_mut().unwrap().value = payload.encode_to_vec();
    let response = handle_request(&gateway, request, None).await;
    assert_eq!(response.status_code, 200);
    let payload = response.payload.unwrap();
    let decoded = DynamicMessage::decode(response_for(GET).unwrap().descriptor(), payload.value.as_slice()).unwrap();
    let state = msg_field(&decoded, "state").unwrap();
    assert_eq!(msg_field(&state, "skin").unwrap().get_field_by_name("no_skin").unwrap().as_bool(), Some(true));
    assert!(crate::dao::ship_skins::insert_license(&db, fixture.character, skin, None).await.unwrap());
    client.session.apply(vec![("stationid", marshal::Value::Int(station))]);
    let app = [31; 16];
    let lease = gateway.notice_hub.lease(app);
    lease.bind(&session, |id| gateway.world.online_session(id));
    let mut stream = gateway.notice_hub.register(app, 2);
    let args = [marshal::Value::Int(ship), marshal::Value::Int(i64::from(skin))];
    let kwargs = marshal::Value::Dict(vec![]);
    let call = crate::services::Call { service: "shipCosmeticsMgr", method: "ApplySkinToShip", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), marshal::Value::None);
    let frame = stream.rx.try_recv().unwrap();
    let notice = crate::gateway::protocol::Notice::decode(&frame[5..]).unwrap();
    assert_eq!(notice.target_group.unwrap().target, Some(Target::Character(u32::try_from(fixture.character).unwrap())));
    let payload = notice.payload.unwrap();
    let decoded = DynamicMessage::decode(message(SET).unwrap(), payload.value.as_slice()).unwrap();
    let state = msg_field(&decoded, "state").unwrap();
    let id = msg_field(&msg_field(&msg_field(&state, "skin").unwrap(), "firstparty").unwrap(), "identifier").unwrap();
    assert_eq!(id.get_field_by_name("sequential").unwrap().as_u64(), Some(u64::try_from(skin).unwrap()));
    assert!(stream.rx.try_recv().is_err());
    let system = client.sde.map.stations().find_map(|s| s.solar_system_id).unwrap();
    gateway.world.system(system).await.unwrap();
    session.station_id = None;
    session.system_id = Some(system);
    gateway.world.set_online_session(fixture.character, Some(session.clone()));
    let response = handle_request(&gateway, make_request(ALL), None).await;
    assert_eq!(response.status_code, 200);
    let payload = response.payload.unwrap();
    let decoded = DynamicMessage::decode(response_for(ALL).unwrap().descriptor(), payload.value.as_slice()).unwrap();
    assert!(decoded.get_field_by_name("states").unwrap().as_list().unwrap().is_empty());
    let response = handle_request(&gateway, make_request(GET), None).await;
    assert_eq!(response.status_code, 404);
    assert_eq!(response.status_message, infinity_core::messages::gateway::NOT_FOUND);
    assert!(response.payload.is_none());
    let observer = crate::services::tests::fixture::create(&db).await.unwrap();
    let outsider = crate::services::tests::fixture::create(&db).await.unwrap();
    let make_ball = |id, character| crate::space::Ball {
        id, type_id: hull, owner_id: character, corp_id: 1, alliance_id: -1, char_id: Some(character),
        signature_radius: Some(1.0), mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0, pos: [character as f64 * 10_000.0, 0.0, 0.0], vel: [0.0; 3],
        old_pos: [character as f64 * 10_000.0, 0.0, 0.0], old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3],
        follow_id: 0, follow_range: 0.0, free: true, global: false, massive: true, interactive: true,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new()
    };
    db.execute("UPDATE game.items SET location_id=$2 WHERE item_id=$1", &[&ship, &system]).await.unwrap();
    for (id, character) in [(ship, fixture.character), (observer.character, observer.character), (outsider.character, outsider.character)] {
        let ball = make_ball(id, character);
        gateway.world.with(system, move |s| s.insert(ball)).await.unwrap();
    }
    let mut observer_session = session.clone();
    observer_session.character_id = observer.character;
    observer_session.user_id = Some(observer.account);
    observer_session.ship_id = Some(observer.character);
    let mut outsider_session = observer_session.clone();
    outsider_session.character_id = outsider.character;
    outsider_session.user_id = Some(outsider.account);
    outsider_session.ship_id = Some(outsider.character);
    session.ship_id = Some(ship);
    gateway.world.set_online_session(fixture.character, Some(session.clone()));
    let mut pilot_receivers = Vec::new();
    for s in [&observer_session, &outsider_session] {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        pilot_receivers.push(rx);
        gateway.world.register_pilot(s.character_id, tx);
        gateway.world.set_online_session(s.character_id, Some(s.clone()));
    }
    let observer_app = [32; 16];
    let observer_lease = gateway.notice_hub.lease(observer_app);
    observer_lease.bind(&observer_session, |id| gateway.world.online_session(id));
    let mut observer_stream = gateway.notice_hub.register(observer_app, 4);
    let outsider_app = [33; 16];
    let outsider_lease = gateway.notice_hub.lease(outsider_app);
    outsider_lease.bind(&outsider_session, |id| gateway.world.online_session(id));
    let mut outsider_stream = gateway.notice_hub.register(outsider_app, 4);
    let (destiny, _destiny_rx) = tokio::sync::mpsc::unbounded_channel();
    gateway.world.observe(system, fixture.character, ship, destiny).await.unwrap();
    let (destiny, _destiny_rx2) = tokio::sync::mpsc::unbounded_channel();
    gateway.world.observe(system, observer.character, observer.character, destiny).await.unwrap();
    let timeout = std::time::Duration::from_secs(15);
    let _ = tokio::time::timeout(timeout, stream.rx.recv()).await.unwrap().unwrap();
    let _ = tokio::time::timeout(timeout, observer_stream.rx.recv()).await.unwrap().unwrap();
    client.session.apply(vec![("stationid", marshal::Value::None), ("solarsystemid", marshal::Value::Int(system)), ("solarsystemid2", marshal::Value::Int(system)), ("shipid", marshal::Value::Int(ship))]);
    let args = [marshal::Value::Int(ship), marshal::Value::None];
    let call = crate::services::Call { service: "shipCosmeticsMgr", method: "ApplySkinToShip", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), marshal::Value::None);
    let mut first_notice = None;
    for (receiver, character) in [(&mut stream.rx, fixture.character), (&mut observer_stream.rx, observer.character)] {
        let frame = receiver.try_recv().unwrap();
        let notice = crate::gateway::protocol::Notice::decode(&frame[5..]).unwrap();
        assert_eq!(notice.target_group.as_ref().unwrap().target, Some(Target::Character(u32::try_from(character).unwrap())));
        let payload = notice.payload.as_ref().unwrap();
        let decoded = DynamicMessage::decode(message(SET).unwrap(), payload.value.as_slice()).unwrap();
        assert_eq!(msg_field(&msg_field(&decoded, "state").unwrap(), "skin").unwrap().get_field_by_name("no_skin").unwrap().as_bool(), Some(true));
        if let Some((uuid, bytes)) = &first_notice {
            assert_eq!(&notice.uuid, uuid);
            assert_eq!(&payload.value, bytes);
        } else { first_notice = Some((notice.uuid.clone(), payload.value.clone())); }
        assert!(receiver.try_recv().is_err());
    }
    assert!(outsider_stream.rx.try_recv().is_err());
    let (cloaked, ready) = tokio::sync::oneshot::channel();
    let generation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(gateway.world.with(system, move |s| s.begin_module_cloak(ship, ship, generation, 0, 0, |_| true, Some(Box::new(move |_| { let _ = cloaked.send(()); })))).await.unwrap());
    tokio::time::timeout(timeout, ready).await.unwrap().unwrap();
    let args = [marshal::Value::Int(ship), marshal::Value::Int(i64::from(skin))];
    let call = crate::services::Call { service: "shipCosmeticsMgr", method: "ApplySkinToShip", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(crate::services::dispatch(&mut client, &call).await.unwrap(), marshal::Value::None);
    let frame = stream.rx.try_recv().unwrap();
    let notice = crate::gateway::protocol::Notice::decode(&frame[5..]).unwrap();
    let payload = notice.payload.unwrap();
    let decoded = DynamicMessage::decode(message(SET).unwrap(), payload.value.as_slice()).unwrap();
    let encoded = msg_field(&decoded, "state").unwrap();
    assert!(msg_field(&encoded, "skin").unwrap().has_field_by_name("firstparty"));
    assert!(observer_stream.rx.try_recv().is_err());
    assert!(outsider_stream.rx.try_recv().is_err());
    gateway.world.unregister_pilot_generation(observer.character, None);
    gateway.world.unregister_pilot_generation(outsider.character, None);
    crate::services::tests::fixture::cleanup(&db, observer).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, outsider).await.unwrap();
    gateway.world.unregister_pilot_generation(fixture.character, None);
    crate::services::tests::fixture::cleanup(&db, fixture).await.unwrap();
}
