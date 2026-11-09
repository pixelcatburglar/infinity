use super::*;

#[test]
fn boards_connect_entry_to_core() {
    for tier in 1..=4 {
        let mut id = 0;
        let (tiles, start) = board(tier, || {
            id += 1;
            id
        });
        let core = tiles.values().filter(|t| t.kind == TYPE_CORE).count();
        assert_eq!(core, 1);
        assert!(!tiles[&start].hidden);
        let g = Game {
            container: 0,
            system: 0,
            tier,
            tiles,
            coherence: 100,
            max_coherence: 100,
            strength: 30,
            inventory: vec![],
            next_id: id,
            skip_click: None,
        };
        assert!(g.distance_to_core(start).is_some_and(|d| d > 0));
        assert!(neighbours(start).iter().any(|n| g.flippable(*n)));
    }
}

#[test]
fn analyzer_skill_matches_postgres_integer_cast() {
    assert_eq!(analyzer_skill(2.5).ok(), Some(2));
    assert_eq!(analyzer_skill(3.5).ok(), Some(4));
    assert_eq!(analyzer_skill(-2.5).ok(), Some(-2));
    assert_eq!(analyzer_skill(i32::MIN as f64).ok(), Some(i32::MIN));
    assert_eq!(analyzer_skill(i32::MAX as f64).ok(), Some(i32::MAX));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, i32::MIN as f64 - 1.0, i32::MAX as f64 + 1.0] {
        assert!(matches!(analyzer_skill(value), Err(CallError::Internal(_))));
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn loaded_analyzers_resolve_their_required_skills() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    for (name, skill) in [("Data Analyzer", SKILL_HACKING), ("Relic Analyzer", SKILL_ARCHAEOLOGY)] {
        let ty = sde.types.iter().find(|t| t.published == Some(true)
            && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
            && t.name.as_deref().is_some_and(|n| n.starts_with(&format!("{name} ")))).expect("published analyzer in SDE");
        assert_eq!(required_analyzer_skill(ty).unwrap(), Some(skill), "{name}");
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn start_requires_space_actor_ship_and_target() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).expect("published SDE ship").type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
        && t.name.as_deref().is_some_and(|name| name.starts_with("Data Analyzer "))
        && matches!(required_analyzer_skill(t), Ok(Some(SKILL_HACKING)))).expect("published data analyzer").type_id;
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let system = owner.character * 100 + 94;
    let created = Arc::new(std::sync::Mutex::new((None, None)));
    let scenario_created = created.clone();
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let ship = crate::dao::items::creation::positioned(&scenario_db, ship_type, owner.character, system, [0.0; 3]).await.unwrap();
        let module = crate::dao::items::creation::rookie_ship(&scenario_db, module_type, owner.character, ship, infinity_core::consts::FLAG_MED_SLOT_FIRST).await.unwrap();
        scenario_db.execute("UPDATE game.items SET online = true WHERE item_id = $1", &[&module]).await.unwrap();
        let container = crate::dao::exploration::insert_container(&scenario_db, 1, system, [0.0; 3]).await.unwrap();
        scenario_created.lock().unwrap().0 = Some(container);
        let site = crate::services::exploration::tests::hacking_site(&scenario_db, system, container).await;
        scenario_created.lock().unwrap().1 = Some(site);
        let world = World::new(scenario_db.clone(), sde.clone(), 0);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut identity = Identity { char_id: owner.character, ship_id: Some(ship), corp_id: 0, station_id: None, system_id: Some(system) };
        let result = start(&identity, &scenario_db, &world, &sde, None, sender.clone(), owner.account, module, Some(container), |_| panic!("unexpected notification")).await.unwrap();
        assert!(matches!(result, Value::None));
        identity.ship_id = None;
        let result = start(&identity, &scenario_db, &world, &sde, Some(system), sender.clone(), owner.account, module, Some(container), |_| panic!("unexpected notification")).await.unwrap();
        assert!(matches!(result, Value::None));
        identity.ship_id = Some(ship);
        identity.char_id = 0;
        let result = start(&identity, &scenario_db, &world, &sde, Some(system), sender.clone(), owner.account, module, Some(container), |_| panic!("unexpected notification")).await.unwrap();
        assert!(matches!(result, Value::None));
        identity.char_id = owner.character;
        let result = start(&identity, &scenario_db, &world, &sde, Some(system), sender, owner.account, module, Some(container), |_| panic!("unexpected notification")).await;
        assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT));
        assert!(receiver.try_recv().is_err());
    }).await;
    let (container, site) = *created.lock().unwrap();
    if let Some(site) = site { crate::services::exploration::tests::remove_hacking_site(&db, site).await; }
    if let Some(container) = container { crate::dao::exploration::delete_container(&db, container).await.unwrap(); }
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn developer_board_request_does_not_create_hack_or_event() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = owner.character;
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "hackingMgr", method: "StartNewGameInstance", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(dispatch(&mut client, &call).await.unwrap().unwrap(), Value::None);
        assert!(!games().contains_key(&character));
        let rows = scenario_db.query("SELECT event_id FROM game.progress_events WHERE kind='ContainerHacked' AND subject_character_id=$1", &[&character]).await.unwrap();
        assert!(rows.is_empty());
    }).await;
    games().remove(&character);
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn winning_hacking_turn_journals_natural_origin() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 95;
    let container = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    let site = crate::services::exploration::tests::hacking_site(&db, system, container).await;
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        client.session.apply(vec![("solarsystemid", Value::Int(system))]);
        let core = Tile { id: 2, kind: TYPE_CORE, sub: CORE_LOW, coord: (0, 0), hidden: false, blocked: false, strength: 1, coherence: 1, max_coherence: 1 };
        games().insert(character, Game { container, system, tier: 1, tiles: HashMap::from([((0, 0), core)]), coherence: 100, max_coherence: 100, strength: 10, inventory: vec![], next_id: 2, skip_click: None });
        let kwargs = Value::Dict(vec![]);
        let args = [Value::tuple([Value::Int(0), Value::Int(0)])];
        let call = Call { service: "hackingMgr", method: "ClickedOnTile", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert_eq!(dispatch(&mut client, &call).await.unwrap().unwrap(), Value::None);
        assert!(!games().contains_key(&character));
        let rows = scenario_db.query("SELECT origin, payload::text AS payload FROM game.progress_events WHERE kind='ContainerHacked' AND subject_character_id=$1", &[&character]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, String>("origin"), "natural");
        let event = crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, "ContainerHacked", &rows[0].get::<_, String>("payload")).unwrap();
        assert!(matches!(event, crate::lua::events::GameEvent::ContainerHacked { container_id, site_id, context, .. } if container_id == container && site_id == site && context.origin == crate::lua::events::resources::FactOrigin::Natural && context.system_id == Some(system)));
    }).await;
    games().remove(&character);
    crate::services::exploration::tests::remove_hacking_site(&db, site).await;
    crate::dao::exploration::delete_container(&db, container).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
