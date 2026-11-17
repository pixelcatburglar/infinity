use super::*;
use crate::services::tests::{client, fixture};
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn epic_journal_dispatch_matches_loaded_rows_for_bound_and_unbound_calls() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let mut expected: BTreeMap<String, BTreeMap<i32, i64>> = BTreeMap::new();
        for row in c.sde.missions.journal() {
            expected.entry(row.key.clone()).or_default().insert(row.mission_id, row.message_id.expect("journal message ID"));
        }
        assert!(!expected.is_empty(), "dedicated SDE must contain journal messages");
        let expected = Value::Dict(expected.into_iter().map(|(key, rows)| (Value::str(key), Value::Dict(rows.into_iter().map(|(id, msg)| (Value::Int(i64::from(id)), Value::Int(msg))).collect()))).collect());
        let agent_id = super::super::agents().keys().next().copied().expect("retail agent");
        let bind = Value::Int(agent_id);
        let kwargs = Value::None;
        let before = fixture::mission_fingerprint(&test_db, f.character).await.unwrap();
        for bound in [false, true] {
            let call = Call { service: "agentMgr", method: "GetMessagesForEpicArcMissions", args: &[], kwargs: &kwargs, bound, bind_params: bound.then_some(&bind) };
            assert!(crate::services::agency::dispatch(&mut c, &call).await.is_none());
            let leaf = dispatch(&mut c, &call).await.unwrap().unwrap();
            let top = crate::services::dispatch(&mut c, &call).await.unwrap();
            assert_eq!(leaf, expected);
            assert_eq!(marshal::loads(&marshal::dumps(&top)).unwrap(), expected);
        }
        assert_eq!(fixture::mission_fingerprint(&test_db, f.character).await.unwrap(), before);
        assert!(c.outbox.is_empty());
    }).await;
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "journal task failed: {result:?}");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn agent_request_admission_uses_skill_adjusted_effective_standing() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let sde = c.sde.clone();
        let agent = super::super::agents().values().find(|a| a.level == 2 && a.type_id == AGENT_TYPE_BASIC
            && [infinity_core::consts::FACTION_AMARR, infinity_core::consts::FACTION_CALDARI, infinity_core::consts::FACTION_GALLENTE, infinity_core::consts::FACTION_MINMATAR].contains(&a.faction_id) && sde.map.station(a.station_id).is_some()
            && !sde.missions.steps().any(|s| s.agent == a.id)).expect("level 2 empire station agent");
        assert!(sde.missions.iter().any(|m| m.template.as_deref() == Some("agent.missionTemplatizedContent_BasicKillMission") && m.has_name && m.has_message("messages.mission.briefing")
            && m.dungeon_id.and_then(|id| sde.dungeons.get(i64::from(id))).is_some_and(|d| d.difficulty == Some(2))), "level 2 kill content in SDE");
        let (kwargs, bind, args, raws) = (Value::None, Value::Int(agent.id), [Value::Int(ACT_REQUEST)], [(agent.faction_id, -2.4), (agent.corp_id, 0.7), (agent.id, 0.0)]);
        let request = Call { service: "agentMgr", method: "DoAction", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        for (from, value) in raws { crate::dao::characters::standings::set(&test_db, f.character, from, value).await.unwrap(); }
        let train = |name: &'static str, level: i32| {
            let (sde, db) = (sde.clone(), test_db.clone());
            async move {
                let ty = sde.types.iter().find(|t| t.lower_name.as_deref() == Some(name)).expect("skill in SDE").type_id;
                let rank = sde.types.attribute(ty, infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT).expect("skill rank");
                crate::dao::skills::training::set_level(&db, f.character, ty, level, crate::services::skills::formulas::sp_for_level(rank, i64::from(level))).await.unwrap();
            }
        };
        dispatch(&mut c, &request).await.unwrap().unwrap();
        assert!(mission_with(&c, agent.id).await.unwrap().is_none());
        train("criminal connections", 5).await;
        dispatch(&mut c, &request).await.unwrap().unwrap();
        assert!(mission_with(&c, agent.id).await.unwrap().is_none());
        for name in ["diplomacy", "connections"] { train(name, 1).await; }
        dispatch(&mut c, &request).await.unwrap().unwrap();
        assert!(mission_with(&c, agent.id).await.unwrap().is_some());
        for (from, value) in raws { assert_eq!(crate::dao::characters::standings::standing(&test_db, f.character, from).await.unwrap(), Some(value)); }
    }).await;
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "admission task failed: {result:?}");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn objective_info_wire_bytes_cover_hand_in_and_kill_completion_states() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let (c, _peer) = client(db, 999_424_242).await;
    let mut pool = agents().values().filter(|a| c.sde.map.station(a.station_id).is_some());
    let (giver, hand) = (pool.next().unwrap(), pool.next().unwrap());
    let st = c.sde.map.station(hand.station_id).unwrap();
    let mission = |kind, dest_agent| Mission { agent_id: giver.id, mission_id: 424_242, name_id: 777_001, briefing_id: 9, dungeon_id: 31_337,
        faction_id: 500_001, state: STATE_ACCEPTED, system_id: 9_999_999_999_999, pos: [1.0, 2.0, 3.0], reward_isk: 1_234_567,
        reward_lp: 4_321, accepted_at: Some(12_345), kind, arc_id: None, dest_agent, cargo: None };
    let site = |m: &Mission| Value::dict([("locationID", Value::Int(m.system_id)), ("locationType", Value::str("dungeon")),
        ("solarsystemID", Value::Int(m.system_id)), ("typeID", Value::Int(i64::from(TYPE_SOLAR_SYSTEM))),
        ("coords", Value::tuple(m.pos.map(Value::Float))), ("agentID", Value::Int(m.agent_id))]);
    let kill = mission(Kind::Kill, None);
    let loc = Value::dict([("locationID", Value::Int(hand.station_id)), ("locationType", Value::str("station")),
        ("solarsystemID", Value::Int(st.solar_system_id.unwrap())), ("typeID", Value::Int(st.type_id.unwrap()))]);
    let expected_talk = Value::dict([
        ("importantStandings", Value::Int(0)), ("completionStatus", Value::Int(0)), ("missionTitleID", Value::Int(777_001)), ("missionState", Value::Int(i64::from(STATE_ACCEPTED))),
        ("objectives", Value::list([Value::tuple([Value::str("agent"), Value::tuple([Value::Int(hand.id), loc])])])), ("dungeons", Value::list([])),
        ("locations", Value::list([Value::Int(hand.system_id)])), ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(1_234_567), Value::None])])), ("loyaltyPoints", Value::Int(4_321)),
        ("researchPoints", Value::Int(0)), ("bonusRewards", Value::list([])), ("collateral", Value::list([])), ("contentID", Value::Int(424_242))]);
    assert_eq!(marshal::dumps(&objective_info(&c, &mission(Kind::Talk, Some(hand.id))).await.unwrap()), marshal::dumps(&expected_talk));
    let dung_open = Value::dict([("dungeonID", Value::Int(31_337)), ("optional", Value::Bool(false)), ("objectiveCompleted", Value::None), ("location", site(&kill)), ("ownerID", Value::Int(500_001))]);
    let dung_done = Value::dict([("dungeonID", Value::Int(31_337)), ("optional", Value::Bool(false)), ("objectiveCompleted", Value::Int(1)), ("location", site(&kill)), ("ownerID", Value::Int(500_001)), ("completionStatus", Value::Int(1))]);
    let expected_kill = Value::dict([
        ("importantStandings", Value::Int(0)), ("completionStatus", Value::Int(0)), ("missionTitleID", Value::Int(777_001)), ("missionState", Value::Int(i64::from(STATE_ACCEPTED))),
        ("objectives", Value::list([])), ("dungeons", Value::list([dung_open])), ("locations", Value::list([Value::Int(9_999_999_999_999)])), ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(1_234_567), Value::None])])), ("loyaltyPoints", Value::Int(4_321)),
        ("researchPoints", Value::Int(0)), ("bonusRewards", Value::list([])), ("collateral", Value::list([])), ("contentID", Value::Int(424_242))]);
    assert_eq!(marshal::dumps(&kill_objective_info(&c, &kill).await), marshal::dumps(&expected_kill));
    let (agent_id, system) = super::super::sites::preflight_clear(&c, &kill).await.unwrap(); super::super::sites::force_clear(&c, agent_id, system).await.unwrap();
    let expected_done = Value::dict([
        ("importantStandings", Value::Int(0)), ("completionStatus", Value::Int(1)), ("missionTitleID", Value::Int(777_001)), ("missionState", Value::Int(i64::from(STATE_ACCEPTED))),
        ("objectives", Value::list([])), ("dungeons", Value::list([dung_done])), ("locations", Value::list([Value::Int(9_999_999_999_999)])), ("agentGift", Value::list([])),
        ("normalRewards", Value::list([Value::tuple([Value::Int(TYPE_CREDITS), Value::Int(1_234_567), Value::None])])), ("loyaltyPoints", Value::Int(4_321)),
        ("researchPoints", Value::Int(0)), ("bonusRewards", Value::list([])), ("collateral", Value::list([])), ("contentID", Value::Int(424_242))]);
    assert_eq!(marshal::dumps(&kill_objective_info(&c, &kill).await), marshal::dumps(&expected_done));
    despawn(&c, 999_424_242, agent_id);
}
