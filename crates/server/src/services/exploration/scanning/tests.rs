use super::*;
use crate::services::tests::fixture;

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
        client.login.role = crate::consts::ROLE_QA;
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
        client.world.unregister_pilot(character);
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
