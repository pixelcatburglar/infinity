use super::*;
use crate::services::tests::fixture;
mod multi_type;
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn refused_probe_recovery_keeps_probe_and_sends_no_notification() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character).await;
    let system = client.sde.map.systems().next().unwrap().solar_system_id;
    let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && ty.capacity.is_some_and(|v| v > 0.0)).unwrap();
    let kind = client.sde.types.iter().find(|ty| ty.volume.is_some_and(|v| v > hull.capacity.unwrap() * 1000.0)).unwrap().type_id;
    let ship = crate::dao::items::transfers::insert(&db, hull.type_id, character, system, 0, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&character, &ship]).await.unwrap();
    client.session.apply(vec![("shipid", Value::Int(ship))]);
    let id = character * 100 + 50;
    state().probes.insert(id, Probe { char_id: character, system, type_id: kind, pos: [0.0; 3], expiry: filetime_now() + FILETIME_SECOND * 60, strength: 1.0 });
    let args = [Value::list([Value::Int(id)])];
    let kwargs = Value::None;
    let call = Call { service: "scanMgr", method: "RecoverProbes", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    let before = client.outbox.len();
    assert!(recover_probes(&mut client, &call, false).await.is_err());
    assert!(state().probes.contains_key(&id));
    assert_eq!(client.outbox.len(), before);
    assert!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Location(ship)).await.unwrap().is_empty());
    state().probes.remove(&id);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn recovery_excludes_expired_probes_and_deposits_live_probe_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character).await;
    let system = client.sde.map.systems().next().unwrap().solar_system_id;
    let hull = client.sde.types.iter().find(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && ty.capacity.is_some_and(|v| v > 0.0)).unwrap();
    let kind = client.sde.types.iter().find(|ty| ty.group_id == Some(infinity_core::consts::GROUP_SCANNER_PROBE) && ty.volume.is_some_and(|v| v > 0.0 && v < hull.capacity.unwrap())).unwrap().type_id;
    let ship = crate::dao::items::transfers::insert(&db, hull.type_id, character, system, 0, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&ship]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&character, &ship]).await.unwrap();
    client.session.apply(vec![("shipid", Value::Int(ship))]);
    let expired = character * 100 + 51;
    let live = character * 100 + 52;
    let now = filetime_now();
    state().probes.insert(expired, Probe { char_id: character, system, type_id: kind, pos: [0.0; 3], expiry: now - 1, strength: 1.0 });
    state().probes.insert(live, Probe { char_id: character, system, type_id: kind, pos: [0.0; 3], expiry: now + FILETIME_SECOND * 60, strength: 1.0 });
    let kwargs = Value::None;
    let expired_args = [Value::list([Value::Int(expired)])];
    let expired_call = Call { service: "scanMgr", method: "RecoverProbes", args: &expired_args, kwargs: &kwargs, bound: true, bind_params: None };
    let before = client.outbox.len();
    assert_eq!(recover_probes(&mut client, &expired_call, false).await.unwrap(), Value::list([]));
    assert_eq!(client.outbox.len(), before);
    assert!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Location(ship)).await.unwrap().is_empty());
    let live_args = [Value::list([Value::Int(live), Value::Int(live)])];
    let live_call = Call { service: "scanMgr", method: "RecoverProbes", args: &live_args, kwargs: &kwargs, bound: true, bind_params: None };
    assert_eq!(recover_probes(&mut client, &live_call, false).await.unwrap(), Value::list([Value::Int(live)]));
    assert_eq!(recover_probes(&mut client, &live_call, false).await.unwrap(), Value::list([]));
    let items = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Location(ship)).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].type_id, kind);
    assert_eq!(items[0].quantity, 1);
    state().probes.remove(&expired);
    state().probes.remove(&live);
    db.execute("DELETE FROM game.items WHERE location_id=$1", &[&ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}


#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn ordinary_scan_removes_disappeared_qa_result() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 10;
    let site_id = system + 1;
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        client.login.role = infinity_core::consts::ROLE_QA;
        client.session.apply(vec![("solarsystemid", Value::Int(system))]);
        client.world.system(system).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(character, tx);
        let mut site = new_site(system, [0.0; 3], Kind::Anomaly, STRENGTH_ALL, 1.0, &mut HashSet::new());
        site.id = site_id;
        site.target = "QA-SELECTED".into();
        {
            let mut st = state();
            st.loaded.insert(system);
            st.sites.insert(site_id, site);
            st.reported.insert((character, system), ["OLD-RESULT".to_string()].into_iter().collect());
        }
        let kwargs = Value::Dict(vec![]);
        let args = [Value::tuple([Value::str("QA-SELECTED")])];
        let qa = Call { service: "scanMgr", method: "QAScanSites", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        let value = crate::services::dispatch(&mut client, &qa).await.unwrap();
        assert_eq!(value.items().unwrap().len(), 1);
        assert_eq!(state().reported.get(&(character, system)).unwrap().len(), 2);
        state().sites.remove(&site_id);
        let args = [Value::None];
        let scan = Call { service: "scanMgr", method: "RequestScans", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &scan).await.unwrap(), Value::None);
        let (event, args) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
        assert_eq!(event, "OnSystemScanStopped");
        let absent = args[2].items().unwrap();
        assert!(absent.contains(&Value::str("QA-SELECTED")));
        assert!(absent.contains(&Value::str("OLD-RESULT")));
        assert!(state().reported.get(&(character, system)).unwrap().is_empty());
        client.world.unregister_pilot_generation(character, None);
    }).await;
    {
        let mut st = state();
        st.sites.remove(&site_id);
        st.loaded.remove(&system);
        st.reported.remove(&(character, system));
    }
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn ordinary_scan_does_not_publish_failed_perfect_results() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 20;
    let first = system + 1;
    let second = system + 2;
    let trigger = format!("scan_ordinary_failure_{character}");
    let function = format!("game.scan_ordinary_failure_{character}");
    let scenario_db = db.clone();
    let scenario_trigger = trigger.clone();
    let scenario_function = function.clone();
    let result = tokio::spawn(async move {
        let sql = format!("CREATE FUNCTION {scenario_function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.character_id = {character} AND NEW.site_id = {second} THEN RAISE EXCEPTION 'forced scan failure'; END IF; RETURN NEW; END $$");
        scenario_db.execute(&sql, &[]).await.unwrap();
        let sql = format!("CREATE TRIGGER {scenario_trigger} BEFORE INSERT ON game.exploration_scanned FOR EACH ROW EXECUTE FUNCTION {scenario_function}()");
        scenario_db.execute(&sql, &[]).await.unwrap();
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        client.session.apply(vec![("solarsystemid", Value::Int(system))]);
        client.world.system(system).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(character, tx);
        let mut taken = HashSet::new();
        let mut a = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_ALL, 1.0, &mut taken);
        let mut b = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_ALL, 1.0, &mut taken);
        a.id = first;
        b.id = second;
        save_site(&scenario_db, &a).await.unwrap();
        save_site(&scenario_db, &b).await.unwrap();
        let targets = [a.target.clone(), b.target.clone()];
        let mut probes = vec![];
        {
            let mut st = state();
            st.loaded.insert(system);
            st.sites.insert(first, a);
            st.sites.insert(second, b);
            for id in system + 3..system + 7 {
                st.probes.insert(id, Probe { char_id: character, system, type_id: 1, pos: [0.0; 3], expiry: filetime_now() + FILETIME_SECOND * 60, strength: 100.0 });
                probes.push((Value::Int(id), keyval([("destination", pos_value([0.0; 3])), ("scanRange", Value::Float(AU))])));
            }
        }
        let kwargs = Value::Dict(vec![]);
        let args = [Value::Dict(probes)];
        let scan = Call { service: "scanMgr", method: "RequestScans", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        assert_eq!(crate::services::dispatch(&mut client, &scan).await.unwrap(), Value::None);
        let (event, args) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
        assert_eq!(event, "OnSystemScanStopped");
        assert!(args[1].items().unwrap().is_empty());
        assert!(!crate::dao::exploration::is_scanned(&scenario_db, character, first).await.unwrap());
        assert!(!crate::dao::exploration::is_scanned(&scenario_db, character, second).await.unwrap());
        let st = state();
        assert!(!st.scanned.contains(&(character, first)));
        assert!(!st.scanned.contains(&(character, second)));
        assert!(!targets.iter().any(|target| st.reported.get(&(character, system)).is_some_and(|set| set.contains(target))));
    }).await;
    db.execute(&format!("DROP TRIGGER IF EXISTS {trigger} ON game.exploration_scanned"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS {function}()"), &[]).await.unwrap();
    db.execute("DELETE FROM game.exploration_scanned WHERE character_id = $1 AND site_id = ANY($2)", &[&character, &vec![first, second]]).await.unwrap();
    delete_site(&db, first).await.unwrap();
    delete_site(&db, second).await.unwrap();
    {
        let mut st = state();
        st.sites.remove(&first);
        st.sites.remove(&second);
        st.loaded.remove(&system);
        st.reported.remove(&(character, system));
        st.scanned.remove(&(character, first));
        st.scanned.remove(&(character, second));
        for id in system + 3..system + 7 {
            st.probes.remove(&id);
        }
    }
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn concurrent_resolution_journals_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let mut site = new_site(character * 100 + 80, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    site.archetype = Some(44);
    site.dungeon_id = Some(12345);
    save_site(&db, &site).await.unwrap();
    let resolved = ResolvedSignature::from(&site);
    site.archetype = Some(45);
    site.dungeon_id = Some(54321);
    let before = filetime_now();
    let context = FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: Some(site.system), station_id: None, origin: FactOrigin::Natural };
    let (first, second) = tokio::join!(
        complete_scans(&db, character, std::slice::from_ref(&resolved), &context),
        complete_scans(&db, character, std::slice::from_ref(&resolved), &context),
    );
    first.unwrap(); second.unwrap();
    let after = filetime_now();
    let rows = db.query("SELECT origin, payload::text AS payload FROM game.progress_events WHERE kind='SignatureResolved' AND subject_character_id=$1", &[&character]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, String>("origin"), "natural");
    let event = crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, "SignatureResolved", &rows[0].get::<_, String>("payload")).unwrap();
    match event {
        crate::lua::events::GameEvent::SignatureResolved { char_id, site_id, signature_id, signature_type, archetype, dungeon_type, occurred_at, context } => {
            assert_eq!((char_id, site_id, signature_id, signature_type), (character, site.id, site.target.clone(), site.strength_attr));
            assert_eq!((archetype, dungeon_type), (Some(44), Some(12345)));
            assert_eq!(context.system_id, Some(site.system));
            assert_eq!(context.origin, FactOrigin::Natural);
            assert!((before..=after).contains(&occurred_at));
        }
        _ => panic!("unexpected event"),
    }
    state().scanned.remove(&(character, site.id));
    delete_site(&db, site.id).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn gm_scan_all_journals_only_first_resolution() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 91;
    let mut site = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut HashSet::new());
    site.id = crate::dao::exploration::next_site_id(&db).await.unwrap();
    save_site(&db, &site).await.unwrap();
    let id = site.id;
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        client.session.apply(vec![("solarsystemid", Value::Int(system))]);
        { let mut st = state(); st.loaded.insert(system); st.sites.insert(id, site); }
        gm_scan_all(&mut client).await.unwrap();
        gm_scan_all(&mut client).await.unwrap();
        let rows = scenario_db.query("SELECT origin, payload::text AS payload FROM game.progress_events WHERE kind='SignatureResolved' AND subject_character_id=$1", &[&character]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, String>("origin"), "gm");
        let event = crate::progress::codec::decode(crate::progress::codec::SCHEMA_VERSION, "SignatureResolved", &rows[0].get::<_, String>("payload")).unwrap();
        assert!(matches!(event, crate::lua::events::GameEvent::SignatureResolved { site_id, context, .. } if site_id == id && context.origin == FactOrigin::Gm));
    }).await;
    { let mut st = state(); st.sites.remove(&id); st.loaded.remove(&system); st.scanned.remove(&(character, id)); st.reported.remove(&(character, system)); }
    delete_site(&db, id).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn ordinary_partial_known_and_anomaly_do_not_journal() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 92;
    let ids = [system + 1, system + 2, system + 3];
    let scenario_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(scenario_db.clone(), character).await;
        client.session.apply(vec![("solarsystemid", Value::Int(system))]);
        client.world.system(system).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(character, tx);
        let mut taken = HashSet::new();
        let mut partial = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut taken);
        let mut known = new_site(system, [0.0; 3], Kind::Signature, STRENGTH_RADAR, 1.0, &mut taken);
        let mut anomaly = new_site(system, [0.0; 3], Kind::Anomaly, STRENGTH_ALL, 1.0, &mut taken);
        partial.id = ids[0]; known.id = ids[1]; anomaly.id = ids[2];
        for site in [&partial, &known, &anomaly] { save_site(&scenario_db, site).await.unwrap(); }
        assert!(crate::dao::exploration::mark_scanned(&scenario_db, character, known.id).await.unwrap());
        { let mut st = state(); st.loaded.insert(system); for site in [partial, known, anomaly] { st.sites.insert(site.id, site); } }
        let args = [Value::None];
        let kwargs = Value::Dict(vec![]);
        let scan = Call { service: "scanMgr", method: "RequestScans", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        crate::services::dispatch(&mut client, &scan).await.unwrap();
        let (event, args) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
        assert_eq!(event, "OnSystemScanStopped");
        assert_eq!(args[1].items().unwrap().len(), 3);
        assert!(!crate::dao::exploration::is_scanned(&scenario_db, character, ids[0]).await.unwrap());
        let rows = scenario_db.query("SELECT event_id FROM game.progress_events WHERE kind='SignatureResolved' AND subject_character_id=$1", &[&character]).await.unwrap();
        assert!(rows.is_empty());
        client.world.unregister_pilot_generation(character, None);
    }).await;
    { let mut st = state(); for id in ids { st.sites.remove(&id); st.scanned.remove(&(character, id)); } st.loaded.remove(&system); st.reported.remove(&(character, system)); }
    for id in ids { delete_site(&db, id).await.unwrap(); }
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
