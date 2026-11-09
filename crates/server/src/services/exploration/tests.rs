use super::*;

pub(in crate::services) async fn hacking_site(db: &Db, system: i64, container: i64) -> i64 {
    let mut site = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(db).await.unwrap();
    site.containers = Some(vec![container]);
    save_site(db, &site).await.unwrap();
    let id = site.id;
    state().sites.insert(id, site);
    id
}

pub(in crate::services) async fn remove_hacking_site(db: &Db, id: i64) {
    state().sites.remove(&id);
    delete_site(db, id).await.unwrap();
}

fn site(pos: V3, ease: f64) -> Site {
    new_site(1, pos, Kind::Signature, STRENGTH_ALL, ease, &mut HashSet::new())
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn fact_context_normalizes_unknown_session_dimensions() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let character = owner.character;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db, character).await;
        for (value, expected) in [(Value::Int(0), None), (Value::None, None), (Value::Int(-1), None), (Value::Int(123), Some(123))] {
            c.session.apply(vec![("shipid", value.clone()), ("solarsystemid", value.clone()), ("stationid", value)]);
            let context = fact_context(&c, FactOrigin::Gm);
            assert_eq!((context.ship_id, context.system_id, context.station_id), (expected, expected, expected));
            assert_eq!(context.origin, FactOrigin::Gm);
        }
        c.session.apply(vec![("shipid", Value::Int(42)), ("solarsystemid", Value::Int(43)), ("stationid", Value::None)]);
        let context = fact_context(&c, FactOrigin::Natural);
        assert_eq!((context.ship_id, context.system_id, context.station_id), (Some(42), Some(43), None));
        assert_eq!(context.origin, FactOrigin::Natural);
    }).await;
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[test]
fn pinpoint_formation_resolves_a_site() {
    let s = site([0.0; 3], 0.6);
    let around = |r: f64| (0..4).map(|i| ([r * 0.4 * (i as f64 - 1.5), 0.0, 0.0], r, 40.0 * 1.1025)).collect::<Vec<_>>();
    assert_eq!(certainty(&s, &around(0.25 * AU)).0, 1.0);
    let wide = certainty(&s, &around(8.0 * AU)).0;
    assert!(wide > 0.0 && wide < 0.25, "{wide}");
    // Three probes never pin it down.
    assert!(certainty(&s, &around(0.25 * AU)[..3]).0 < 0.75);
}

#[test]
fn target_ids_look_like_retail() {
    let id = target_id(&HashSet::new());
    assert_eq!(id.len(), 7);
    assert_eq!(&id[3..4], "-");
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn durable_hack_claims_survive_stale_site_saves() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    let mut site = site([0.0; 3], 1.0);
    site.id = id;
    site.containers = Some(vec![101, 102]);
    save_site(&db, &site).await.unwrap();
    assert!(crate::dao::exploration::claim_hacked_container(&db, id, 103).await.unwrap().is_none());
    let first = crate::dao::exploration::claim_hacked_container(&db, id, 101).await.unwrap().unwrap();
    assert_eq!(first.hacked, vec![101]);
    assert!(crate::dao::exploration::claim_hacked_container(&db, id, 101).await.unwrap().is_none());
    save_site(&db, &site).await.unwrap();
    let second = crate::dao::exploration::claim_hacked_container(&db, id, 102).await.unwrap().unwrap();
    assert_eq!(second.hacked, vec![101, 102]);
    delete_site(&db, id).await.unwrap();
    assert!(crate::dao::exploration::claim_hacked_container(&db, id, 102).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn hacked_empty_containers_journal_once_and_preserve_claims() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde, 0);
    let system = owner.character * 100 + 90;
    let a = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    let b = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    let mut site = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    site.containers = Some(vec![a, b]);
    save_site(&db, &site).await.unwrap();
    state().sites.insert(site.id, site.clone());
    let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(system), station_id: None, origin: FactOrigin::Natural };
    let (first, duplicate) = tokio::join!(
        container_hacked(&db, &world, a, owner.character, context.clone()),
        container_hacked(&db, &world, a, owner.character, context.clone()),
    );
    first.unwrap(); duplicate.unwrap();
    save_site(&db, &site).await.unwrap();
    container_hacked(&db, &world, b, owner.character, context).await.unwrap();
    let rows = db.query("SELECT payload::text AS payload FROM game.progress_events WHERE kind='ContainerHacked' AND subject_character_id=$1", &[&owner.character]).await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let event = crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, "ContainerHacked", &row.get::<_, String>("payload")).unwrap();
        assert!(matches!(event, crate::lua::events::GameEvent::ContainerHacked { container_id, container_type: 1, site_id, hack_kind: HackKind::Data, .. } if (container_id == a || container_id == b) && site_id == site.id));
    }
    let stored = crate::dao::exploration::sites(&db, system).await.unwrap();
    assert_eq!(stored[0].hacked, vec![a, b]);
    state().sites.remove(&site.id);
    delete_site(&db, site.id).await.unwrap();
    crate::dao::exploration::delete_container(&db, a).await.unwrap();
    crate::dao::exploration::delete_container(&db, b).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn distinct_hackers_race_empty_and_nonempty_containers() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let first = crate::services::tests::fixture::create(&db).await.unwrap();
    let second = crate::services::tests::fixture::create(&db).await.unwrap();
    let system = first.character * 100 + 93;
    let empty = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    let full = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    let ship = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    crate::dao::exploration::insert_loot(&db, 1, full, 1).await.unwrap();
    let mut site = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    site.archetype = Some(45);
    site.containers = Some(vec![empty, full]);
    save_site(&db, &site).await.unwrap();
    let id = site.id;
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(Sde::load(&scenario_db).await.unwrap());
        let world = World::new(scenario_db.clone(), sde, 0);
        state().sites.insert(id, site);
        let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: Some(ship), ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Natural };
        for container in [empty, full] {
            let (a, b) = tokio::join!(
                container_hacked(&scenario_db, &world, container, first.character, context.clone()),
                container_hacked(&scenario_db, &world, container, second.character, context.clone()),
            );
            a.unwrap(); b.unwrap();
            let rows = scenario_db.query("SELECT subject_character_id, origin, payload::text AS payload FROM game.progress_events WHERE kind='ContainerHacked' AND subject_character_id = ANY($1)", &[&vec![first.character, second.character]]).await.unwrap();
            let events: Vec<_> = rows.iter().filter_map(|row| {
                let event = crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, "ContainerHacked", &row.get::<_, String>("payload")).unwrap();
                match event {
                    crate::lua::events::GameEvent::ContainerHacked { char_id, container_id, container_type, site_id, archetype, hack_kind, occurred_at, context } if container_id == container => {
                        assert_eq!(row.get::<_, i64>("subject_character_id"), char_id);
                        assert_eq!(row.get::<_, String>("origin"), "natural");
                        assert_eq!((container_type, site_id, archetype, hack_kind), (1, id, Some(45), HackKind::Data));
                        assert!(occurred_at > 0);
                        assert_eq!((context.corporation_id, context.alliance_id, context.faction_id), (Some(1), None, None));
                        assert_eq!((context.ship_id, context.ship_type_id, context.system_id, context.station_id, context.origin), (Some(ship), Some(1), Some(system), None, FactOrigin::Natural));
                        Some(char_id)
                    }
                    _ => None,
                }
            }).collect();
            assert_eq!(events.len(), 1);
            assert!(events[0] == first.character || events[0] == second.character);
            if container == full {
                let loot = scenario_db.query_one("SELECT owner_id FROM game.items WHERE location_id=$1", &[&full]).await.unwrap();
                assert_eq!(loot.get::<_, i64>("owner_id"), events[0]);
            }
        }
        let stored = crate::dao::exploration::sites(&scenario_db, system).await.unwrap();
        assert_eq!(stored[0].hacked, vec![empty, full]);
        assert_eq!(state().sites.get(&id).unwrap().hacked, vec![empty, full]);
    }).await;
    state().sites.remove(&id);
    delete_site(&db, id).await.unwrap();
    for item in [empty, full, ship] { crate::dao::exploration::delete_container(&db, item).await.unwrap(); }
    crate::services::tests::fixture::cleanup(&db, first).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, second).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn hack_postclaim_error_and_stale_membership_do_not_publish() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = crate::services::tests::fixture::create(&db).await.unwrap();
    let system = owner.character * 100 + 94;
    let container = crate::dao::exploration::insert_container(&db, 1, system, [0.0; 3]).await.unwrap();
    crate::dao::exploration::insert_loot(&db, 1, container, 1).await.unwrap();
    let mut site = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_ALL, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    site.containers = Some(vec![container]);
    save_site(&db, &site).await.unwrap();
    let id = site.id;
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let world = World::new(scenario_db.clone(), Arc::new(Sde::load(&scenario_db).await.unwrap()), 0);
        state().sites.insert(id, site.clone());
        let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(system), station_id: None, origin: FactOrigin::Natural };
        assert!(container_hacked(&scenario_db, &world, container, owner.character, context.clone()).await.is_err());
        let mut durable = crate::dao::exploration::sites(&scenario_db, system).await.unwrap();
        assert!(durable.remove(0).hacked.is_empty());
        assert!(state().sites.get(&id).unwrap().hacked.is_empty());
        let loot = scenario_db.query_one("SELECT owner_id FROM game.items WHERE location_id=$1", &[&container]).await.unwrap();
        assert_eq!(loot.get::<_, i64>("owner_id"), 1);
        let mut replacement = site.clone();
        replacement.containers = Some(vec![]);
        save_site(&scenario_db, &replacement).await.unwrap();
        container_hacked(&scenario_db, &world, container, owner.character, context).await.unwrap();
        let durable = crate::dao::exploration::sites(&scenario_db, system).await.unwrap();
        assert!(durable[0].hacked.is_empty());
        assert_eq!(durable[0].containers, Some(vec![]));
        let cached = state().sites.get(&id).unwrap().clone();
        assert_eq!(cached.containers, Some(vec![container]));
        assert!(cached.hacked.is_empty());
        let loot = scenario_db.query_one("SELECT owner_id FROM game.items WHERE location_id=$1", &[&container]).await.unwrap();
        assert_eq!(loot.get::<_, i64>("owner_id"), 1);
        let rows = scenario_db.query("SELECT event_id FROM game.progress_events WHERE kind='ContainerHacked' AND subject_character_id=$1", &[&owner.character]).await.unwrap();
        assert!(rows.is_empty());
    }).await;
    state().sites.remove(&id);
    delete_site(&db, id).await.unwrap();
    crate::dao::exploration::delete_container(&db, container).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
