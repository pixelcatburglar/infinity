use super::*;
use crate::dao::missions as dao;
use crate::services::tests::{client, fixture};

async fn availability(c: &mut Client, agent: i64, expected: bool) {
    let args = [Value::Int(agent)];
    let kwargs = Value::None;
    let call = Call { service: "epicArcStatus", method: "AgentHasEpicMissionsForCharacter", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    let before = fixture::mission_fingerprint(&c.db, c.session.int("charid").unwrap()).await.unwrap();
    let result = crate::services::dispatch(c, &call).await.unwrap();
    assert_eq!(marshal::loads(&marshal::dumps(&result)).unwrap(), Value::Bool(expected));
    assert_eq!(fixture::mission_fingerprint(&c.db, c.session.int("charid").unwrap()).await.unwrap(), before);
    assert!(c.outbox.is_empty());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn availability_uses_character_progress_without_mutation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let other = match fixture::create(&db).await { Ok(f) => f, Err(e) => { fixture::cleanup(&db, f).await.unwrap(); panic!("second fixture: {e:?}") } };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let (mut c2, _peer2) = client(test_db.clone(), other.character).await;
        let steps: Vec<_> = c.sde.missions.steps().collect();
        let start = steps.iter().find(|s| s.start && agent(s.agent).is_some()).expect("SDE needs a retail start agent");
        let start_agent = start.agent;
        let target = steps.iter().find(|s| !s.start && agent(s.agent).is_some() && c.sde.missions.get(s.mission_id).is_some() && !steps.iter().any(|x| x.agent == s.agent && x.start)).expect("SDE needs a non-start-only retail agent");
        let (id, arc, target_agent) = (target.mission_id, target.arc, target.agent);
        let branch = steps.iter().find_map(|parent| {
            let targets: Vec<_> = parent.next.iter().filter_map(|id| c.sde.missions.step(*id)).filter(|s| agent(s.agent).is_some() && !steps.iter().any(|x| x.agent == s.agent && x.start)).collect();
            targets.iter().find_map(|a| targets.iter().find(|b| b.agent != a.agent).map(|b| (a.mission_id, a.arc, a.agent, b.agent)))
        }).or_else(|| {
            steps.iter().find_map(|a| {
                if a.start || agent(a.agent).is_none() || steps.iter().any(|x| x.agent == a.agent && x.start) { return None; }
                steps.iter().find(|b| !b.start && b.arc == a.arc && b.agent != a.agent && agent(b.agent).is_some() && !steps.iter().any(|x| x.agent == b.agent && x.start))
                    .map(|b| (a.mission_id, a.arc, a.agent, b.agent))
            })
        }).expect("SDE needs distinct non-start retail agents in one arc");
        let non_epic = super::super::agents().keys().copied().find(|id| !steps.iter().any(|s| s.agent == *id)).expect("SDE needs a non-epic retail agent");
        drop(steps);
        availability(&mut c, start_agent, true).await;
        availability(&mut c, target_agent, false).await;
        availability(&mut c, non_epic, false).await;
        availability(&mut c, -1, false).await;
        dao::set_arc_state(&test_db, f.character, arc, id, "unlocked", None).await.unwrap();
        availability(&mut c, target_agent, true).await;
        availability(&mut c2, target_agent, false).await;
        dao::set_arc_state(&test_db, f.character, arc, id, "completed", Some(1)).await.unwrap();
        availability(&mut c, target_agent, false).await;
        dao::set_arc_state(&test_db, f.character, arc, id, "quit", Some(1)).await.unwrap();
        availability(&mut c, target_agent, false).await;
        let agent_data = agent(target_agent).unwrap();
        let offer = dao::Offer { character_id: f.character, agent_id: target_agent, mission_id: id, dungeon_id: 0, faction_id: 0, state: STATE_OFFERED, system_id: agent_data.system_id, pos: [0.0; 3], reward_isk: 0, reward_lp: 0, offered_at: 1 };
        dao::offer_arc(&test_db, &dao::ArcOffer { offer, kind: "kill", arc_id: arc, dest_agent: None, cargo_type: None, cargo_qty: None }).await.unwrap();
        availability(&mut c, target_agent, true).await;
        assert_eq!(dao::accept_offered(&test_db, f.character, target_agent, id, STATE_OFFERED, STATE_ACCEPTED, 1).await.unwrap(), 1);
        availability(&mut c, target_agent, true).await;
        dao::remove(&test_db, f.character, target_agent).await.unwrap();
        availability(&mut c, target_agent, false).await;
        dao::set_arc_state(&test_db, other.character, branch.1, branch.0, "unlocked", None).await.unwrap();
        availability(&mut c2, branch.2, true).await;
        availability(&mut c2, branch.3, false).await;
        availability(&mut c, branch.2, false).await;
        let kwargs = Value::None;
        let qa_args = [Value::Int(target_agent)];
        let qa = Call { service: "agentMgr", method: "GetPlausibleMissionIDs", args: &qa_args, kwargs: &kwargs, bound: false, bind_params: None };
        let value = crate::services::dispatch(&mut c, &qa).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), Value::list([]));
        let bad = [Value::str("bad")];
        for args in [&[][..], &bad[..]] {
            let call = Call { service: "epicArcStatus", method: "AgentHasEpicMissionsForCharacter", args, kwargs: &kwargs, bound: false, bind_params: None };
            assert!(matches!(crate::services::dispatch(&mut c, &call).await, Err(CallError::User { .. })));
        }
        let arg = [Value::Int(target_agent)];
        let call = Call { service: "epicArcStatus", method: "AgentHasEpicMissionsForCharacter", args: &arg, kwargs: &kwargs, bound: false, bind_params: None };
        c.session.apply(vec![("charid", Value::None)]);
        assert!(matches!(crate::services::dispatch(&mut c, &call).await, Err(CallError::User { .. })));
        let unknown = Call { service: "epicArcStatus", method: "Unknown", args: &arg, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(dispatch(&c, &unknown).await.is_none());
    }).await;
    fixture::cleanup(&db, f).await.unwrap();
    fixture::cleanup(&db, other).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert_eq!(fixture::remaining(&db, other).await.unwrap(), 0);
    assert!(result.is_ok(), "availability task failed: {result:?}");
}
