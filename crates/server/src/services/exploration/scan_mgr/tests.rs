//! RPC authorization, selection and durable completion for selected exploration scans.
use super::*;
use crate::services::tests::fixture;

async fn rpc(c: &mut Client, method: &str, args: &[Value], kwargs: &Value) -> CallResult {
    crate::services::dispatch(c, &Call { service: "scanMgr", method, args, kwargs, bound: true, bind_params: None }).await
}

fn denied(result: CallResult) {
    assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == messages::MSG_CUSTOM_NOTIFY));
}

async fn cleanup(db: &Db, chars: &[i64], systems: &[i64], ids: &[i64]) {
    {
        let mut st = state();
        st.sites.retain(|id, _| !ids.contains(id));
        st.loaded.retain(|system| !systems.contains(system));
        st.scanned.retain(|(character, _)| !chars.contains(character));
        st.reported.retain(|(character, _), _| !chars.contains(character));
    }
    for id in ids { delete_site(db, *id).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn qa_dispatch_validates_role_session_arguments_and_binding() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 20;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db, character).await;
        let empty = Value::Dict(vec![]);
        let args = [Value::tuple([])];
        c.session.apply(vec![("solarsystemid", Value::Int(system))]);
        for role in [0, crate::consts::GM_ROLES] {
            c.login.role = role;
            denied(rpc(&mut c, "QAScanSites", &args, &empty).await);
        }
        c.login.role = crate::consts::ROLE_QA;
        for (ch, space, docked) in [(0, system, 0), (character, 0, system), (character, -1, 0)] {
            c.session.apply(vec![("charid", Value::Int(ch)), ("solarsystemid", Value::Int(space)), ("solarsystemid2", Value::Int(docked))]);
            denied(rpc(&mut c, "QAScanSites", &args, &empty).await);
        }
        c.session.apply(vec![("charid", Value::Int(character)), ("solarsystemid", Value::Int(system))]);
        for bad in [vec![], vec![Value::list([])], vec![Value::Int(1)], vec![Value::tuple([Value::Int(1)])], vec![Value::tuple([Value::tuple([Value::str("X")])])], vec![Value::tuple([]), Value::tuple([])]] {
            denied(rpc(&mut c, "QAScanSites", &bad, &empty).await);
        }
        denied(rpc(&mut c, "QAScanSites", &args, &Value::Dict(vec![(Value::str("x"), Value::Int(1))])).await);
        assert_eq!(rpc(&mut c, "QAScanSites", &args, &empty).await.unwrap(), Value::list([]));
        assert!(!state().loaded.contains(&system));
        assert!(!state().reported.contains_key(&(character, system)));
        state().loaded.insert(system);
        assert_eq!(rpc(&mut c, "QAScanSites", &[Value::tuple([Value::str("UNKNOWN")])], &empty).await.unwrap(), Value::list([]));
        let wire = rpc(&mut c, "GetSystemScanMgr", &[], &empty).await.unwrap();
        let decoded = marshal::loads(&marshal::dumps(&wire)).unwrap();
        let Value::Callback(payload) = decoded else { panic!("expected callback") };
        let reference = marshal::loads(payload.as_bytes().unwrap()).unwrap();
        let oid = reference.items().unwrap()[0].as_text().unwrap();
        assert_eq!(c.bound.get(oid.as_ref()), Some(&("scanMgr".to_string(), Value::None)));
    }).await;
    cleanup(&db, &[character], &[system], &[]).await;
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn qa_selection_wire_persistence_and_character_isolation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let other = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 30;
    let other_system = system + 1;
    let first = crate::dao::exploration::next_site_id(&db).await.unwrap();
    let ids: Vec<i64> = (first..first + 5).collect();
    let test_ids = ids.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), character).await;
        c.login.role = crate::consts::ROLE_QA;
        c.session.apply(vec![("solarsystemid", Value::Int(system))]);
        let mut sites = Vec::new();
        for (i, (sys, kind, target)) in [(system, Kind::Signature, "SELECTED"), (system, Kind::Signature, "UNSELECTED"), (system, Kind::Anomaly, "ANOMALY"), (other_system, Kind::Signature, "CROSS"), (system, Kind::Signature, "EXPIRED")].into_iter().enumerate() {
            let mut site = new_site(sys, [12.5, -3.0, 99.0], kind, STRENGTH_RADAR, 1.0, &mut HashSet::new());
            site.id = test_ids[i]; site.target = target.into();
            site.dungeon_id = Some(123); site.name_id = Some(456); site.archetype = Some(789); site.faction = Some(42);
            if target == "EXPIRED" {
                site.wormhole = Some(Wormhole { type_id: 1, dest_system: other_system, dest_class: 1, other: 1, born: 0, life_minutes: 1.0, mass_max: 1.0, mass_used: 0.0, jump_mass: 1.0, nebula: 1 });
            }
            save_site(&test_db, &site).await.unwrap();
            sites.push(site);
        }
        {
            let mut st = state();
            st.loaded.extend([system, other_system]);
            for site in &sites { st.sites.insert(site.id, site.clone()); }
            st.reported.insert((character, system), ["OLD".to_string()].into_iter().collect());
        }
        let args = [Value::tuple([Value::str("ANOMALY"), Value::str("SELECTED"), Value::str("SELECTED"), Value::str("CROSS"), Value::str("EXPIRED"), Value::str("REMOVED"), Value::str("SHIP")])];
        let value = rpc(&mut c, "QAScanSites", &args, &Value::Dict(vec![])).await.unwrap();
        let decoded = marshal::loads(&marshal::dumps(&value)).unwrap();
        let rows = decoded.items().unwrap();
        assert_eq!(rows.len(), 2);
        for (row, site) in rows.iter().zip([&sites[0], &sites[2]]) {
            for (key, expected) in [("id", Value::str(&site.target)), ("certainty", Value::Float(1.0)), ("pos", Value::tuple(site.pos.map(Value::Float))), ("data", Value::tuple(site.pos.map(Value::Float))), ("dungeonID", Value::Int(123)), ("dungeonNameID", Value::Int(456)), ("archetypeID", Value::Int(789)), ("factionID", Value::Int(42))] {
                assert_eq!(crate::services::value_field(row, key), Some(&expected));
            }
        }
        assert_eq!(state().reported.get(&(character, system)).unwrap(), &["OLD".to_string(), "SELECTED".to_string(), "ANOMALY".to_string()].into_iter().collect());
        assert!(c.outbox.is_empty());
        for (i, expected) in [(0, true), (1, false), (2, false), (3, false), (4, false)] {
            assert_eq!(crate::dao::exploration::is_scanned(&test_db, character, test_ids[i]).await.unwrap(), expected);
            assert!(!crate::dao::exploration::is_scanned(&test_db, other.character, test_ids[i]).await.unwrap());
        }
        state().scanned.retain(|(ch, _)| *ch != character);
        assert_eq!(scan_result_position(&test_db, system, character, "SELECTED").await.unwrap(), Some(sites[0].pos));
        assert_eq!(scan_result_position(&test_db, system, character, "UNSELECTED").await.unwrap(), None);
        assert_eq!(scan_result_position(&test_db, system, other.character, "SELECTED").await.unwrap(), None);
    }).await;
    cleanup(&db, &[character, other.character], &[system, other_system], &ids).await;
    fixture::cleanup(&db, owner).await.unwrap(); fixture::cleanup(&db, other).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn qa_batch_failure_rolls_back_and_retry_commits() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let character = owner.character;
    let system = character * 100 + 40;
    let first = crate::dao::exploration::next_site_id(&db).await.unwrap();
    let ids = vec![first, first + 1];
    let trigger = format!("qa_scan_fail_{character}");
    let function = format!("qa_scan_fail_fn_{character}");
    let test_db = db.clone(); let test_ids = ids.clone();
    let test_trigger = trigger.clone(); let test_function = function.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = crate::services::tests::client(test_db.clone(), character).await;
        c.login.role = crate::consts::ROLE_QA;
        c.session.apply(vec![("solarsystemid", Value::Int(system))]);
        for (id, target) in test_ids.iter().zip(["FIRST", "SECOND"]) {
            let mut site = new_site(system, [1.0, 2.0, 3.0], Kind::Signature, STRENGTH_RADAR, 1.0, &mut HashSet::new());
            site.id = *id; site.target = target.into();
            save_site(&test_db, &site).await.unwrap();
            state().sites.insert(*id, site);
        }
        { let mut st = state(); st.loaded.insert(system); st.reported.insert((character, system), ["OLD".to_string()].into_iter().collect()); }
        test_db.execute(&format!("CREATE FUNCTION game.{test_function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.character_id = {character} AND NEW.site_id = {} THEN RAISE EXCEPTION 'scan fixture failure'; END IF; RETURN NEW; END $$", test_ids[1]), &[]).await.unwrap();
        test_db.execute(&format!("CREATE TRIGGER {test_trigger} BEFORE INSERT ON game.exploration_scanned FOR EACH ROW EXECUTE FUNCTION game.{test_function}()"), &[]).await.unwrap();
        let args = [Value::tuple([Value::str("SECOND"), Value::str("FIRST")])];
        let kwargs = Value::Dict(vec![]);
        assert!(rpc(&mut c, "QAScanSites", &args, &kwargs).await.is_err());
        for id in &test_ids {
            assert!(!crate::dao::exploration::is_scanned(&test_db, character, *id).await.unwrap());
            assert!(!state().scanned.contains(&(character, *id)));
        }
        assert_eq!(state().reported.get(&(character, system)).unwrap(), &["OLD".to_string()].into_iter().collect());
        test_db.execute(&format!("DROP TRIGGER {test_trigger} ON game.exploration_scanned"), &[]).await.unwrap();
        assert_eq!(rpc(&mut c, "QAScanSites", &args, &kwargs).await.unwrap().items().unwrap().len(), 2);
        for id in &test_ids { assert!(crate::dao::exploration::is_scanned(&test_db, character, *id).await.unwrap()); }
    }).await;
    db.execute(&format!("DROP TRIGGER IF EXISTS {trigger} ON game.exploration_scanned"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS game.{function}()"), &[]).await.unwrap();
    cleanup(&db, &[character], &[system], &ids).await;
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
