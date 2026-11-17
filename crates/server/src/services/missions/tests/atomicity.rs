use crate::services::{missions, Call, CallError};
use crate::services::fit::Fit;
use crate::services::tests::{client, client_with_sde, database_message, fixture};
use marshal::Value;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn mission_acceptance_rolls_back_arc_and_offer_when_cargo_write_fails() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let step = c.sde.missions.steps().find(|step| missions::agent(step.agent).is_some() && c.sde.missions.get(step.mission_id).is_some()).expect("dedicated SDE must contain an arc step with a retail agent");
        let agent = missions::agent(step.agent).unwrap();
        fixture::seed_mission(&test_db, &c.sde, f.character, agent.id, agent.station_id, agent.system_id, step.mission_id, step.arc).await.unwrap();
        let trigger = format!("mission_cargo_failure_{}", f.character);
        test_db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'mission cargo write failure'; END $$"), &[]).await.unwrap();
        test_db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT OR UPDATE ON game.items FOR EACH ROW WHEN (NEW.owner_id = {}) EXECUTE FUNCTION game.{trigger}()", f.character), &[]).await.unwrap();
        let before = fixture::mission_fingerprint(&test_db, f.character).await.unwrap();
        let kwargs = Value::None;
        let bind = Value::Int(agent.id);
        let args = [Value::Int(3)];
        let call = Call { service: "agentMgr", method: "DoAction", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        let failed = missions::dispatch(&mut c, &call).await.unwrap();
        test_db.execute(&format!("DROP TRIGGER {trigger} ON game.items"), &[]).await.unwrap();
        test_db.execute(&format!("DROP FUNCTION game.{trigger}()"), &[]).await.unwrap();
        match failed {
            Err(CallError::Internal(error)) => assert_eq!(database_message(&error), Some("mission cargo write failure")),
            _ => panic!("courier write failure was not propagated"),
        }
        assert_eq!(fixture::mission_fingerprint(&test_db, f.character).await.unwrap(), before);
        assert!(c.outbox.is_empty());
        fixture::clear_cargo_stack(&test_db, f.character).await.unwrap();
        let result = missions::dispatch(&mut c, &call).await.unwrap();
        assert!(result.is_ok(), "second mission acceptance failed: {result:?}");
        assert!(!c.outbox.is_empty());
        let accepted = fixture::mission_fingerprint(&test_db, f.character).await.unwrap();
        c.outbox.clear();
        let _ = missions::dispatch(&mut c, &call).await.unwrap();
        assert_eq!(fixture::mission_fingerprint(&test_db, f.character).await.unwrap(), accepted);
        assert!(c.outbox.is_empty());
    }).await;
    let trigger = format!("mission_cargo_failure_{}", f.character);
    db.execute(&format!("DROP TRIGGER IF EXISTS {trigger} ON game.items"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS game.{trigger}()"), &[]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "mission rollback task failed: {result:?}");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn accepted_mission_reward_is_guarded_and_reacceptance_earns_again() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (c, _peer) = client(test_db.clone(), f.character).await;
        let step = c.sde.missions.steps().find(|step| missions::agent(step.agent).is_some_and(|a| a.corp_id != infinity_core::consts::CORP_HERALDRY && c.sde.map.station(a.station_id).is_some_and(|station| station.solar_system_id == Some(a.system_id))) && c.sde.missions.get(step.mission_id).is_some_and(|m| m.reward_quantity.is_some() && m.reward_type.is_some_and(|t| t != missions::TYPE_CREDITS as i32 && c.sde.types.get(t).is_some() && c.sde.is_tradable(t)) && !m.template.as_deref().is_some_and(|t| t.ends_with("TradeMission")))).expect("arc step with tradable item reward at a known agent station");
        let agent = missions::agent(step.agent).unwrap();
        let mission_id = step.mission_id;
        let reward = c.sde.missions.get(mission_id).unwrap();
        let reward_type = reward.reward_type.unwrap();
        let original: String = test_db.query_one("SELECT data::text AS data FROM sde_native.missions WHERE key::int=$1", &[&mission_id]).await.unwrap().get("data");
        let wide = i64::from(i32::MAX) + 1;
        test_db.execute("UPDATE sde_native.missions SET data=jsonb_set(jsonb_set(data,'{missionRewards,reward,rewardQuantity}',to_jsonb($2::bigint)),'{fixedLpRewardOmega}',to_jsonb(7)) WHERE key::int=$1", &[&mission_id, &wide]).await.unwrap();
        let client_db = test_db.clone();
        let loaded = tokio::spawn(async move {
            // The shared client above already published the original SDE
            // before the temporary UPDATE; this fresh load bypasses the
            // process cache so parallel shared callers keep the original.
            let sde = std::sync::Arc::new(crate::sde::Sde::load(&client_db).await.unwrap());
            client_with_sde(client_db, f.character, sde).await
        }).await;
        test_db.execute("UPDATE sde_native.missions SET data=$2::text::jsonb WHERE key::int=$1", &[&mission_id, &original]).await.unwrap();
        let (mut c, _peer) = loaded.unwrap();
        test_db.execute("UPDATE game.characters SET station_id=$2, solar_system_id=$3 WHERE character_id=$1", &[&f.character, &agent.station_id, &agent.system_id]).await.unwrap();
        c.session.apply(vec![("stationid", Value::Int(agent.station_id)), ("solarsystemid2", Value::Int(agent.system_id))]);
        let kwargs = Value::None;
        let bind = Value::Int(agent.id);
        let args = [Value::Int(3)];
        let call = Call { service: "agentMgr", method: "DoAction", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        let at = crate::client::filetime_now();
        let fit = Fit::load_character_at(&test_db, c.sde.clone(), f.character, at).await.unwrap();
        for earned in [1_i64, 2] {
            fixture::clear_cargo_stack(&test_db, f.character).await.unwrap();
            super::super::mechanics::offer_arc(&c, agent, mission_id, &fit, at).await.unwrap();
            assert!(missions::dispatch(&mut c, &call).await.unwrap().is_ok());
            let mission = super::super::mechanics::mission_with(&c, agent.id).await.unwrap().unwrap();
            super::super::mechanics::finish(&mut c, &mission, agent, None).await.unwrap();
            let row = test_db.query_one("SELECT quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4", &[&f.character, &agent.station_id, &infinity_core::consts::FLAG_HANGAR, &reward_type]).await.unwrap();
            assert_eq!(row.get::<_, i64>("quantity"), wide);
            assert!(super::super::mechanics::finish(&mut c, &mission, agent, None).await.is_err());
            assert_eq!(crate::services::standings::lp_balance(&test_db, f.character, agent.corp_id).await.unwrap(), earned * 7);
            let completed: i64 = test_db.query_one("SELECT count(*) AS count FROM game.standing_history WHERE char_id=$1 AND from_id=$2 AND event_type_id=73", &[&f.character, &agent.id]).await.unwrap().get("count");
            assert_eq!(completed, earned, "one completion history row per finish");
            let events = crate::db::outbox::tests::events(&test_db, "LoyaltyPointsEarned", f.character).await;
            assert_eq!(events.len(), earned as usize);
            assert!(events.iter().all(|event| matches!(event, crate::lua::events::GameEvent::LoyaltyPointsEarned {
                char_id, issuer_id, amount: 7, reason: crate::lua::events::resources::LoyaltyReason::MissionReward, context, ..
            } if *char_id == f.character && *issuer_id == agent.corp_id && context.origin == crate::lua::events::resources::FactOrigin::Natural)));
        }
    }).await;
    db.execute("DELETE FROM game.lp_balances WHERE char_id=$1", &[&f.character]).await.unwrap();
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn declined_and_quit_missions_end_atomically_and_journal_event_kind() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        // Copy the step scalars now; the SDE borrow must end before mutable dispatch.
        let (mission_id, arc, retry, agent_id) = {
            let step = c.sde.missions.steps().find(|step| missions::agent(step.agent).is_some() && c.sde.missions.get(step.mission_id).is_some()).expect("dedicated SDE must contain an arc step with a retail agent");
            (step.mission_id, step.arc, step.fail.unwrap_or(step.mission_id), step.agent)
        };
        let agent = missions::agent(agent_id).unwrap();
        let kwargs = Value::None;
        let bind = Value::Int(agent.id);
        for (action, quit, event) in [(4_i64, false, 75_i64), (6, true, 74)] {
            fixture::seed_mission(&test_db, &c.sde, f.character, agent.id, agent.station_id, agent.system_id, mission_id, arc).await.unwrap();
            if quit {
                test_db.execute("UPDATE game.agent_missions SET state=2 WHERE character_id=$1 AND agent_id=$2", &[&f.character, &agent.id]).await.unwrap();
            }
            let observed: i32 = test_db.query_one("SELECT state FROM game.agent_missions WHERE character_id=$1 AND agent_id=$2", &[&f.character, &agent.id]).await.unwrap().get("state");
            let arcs_before: i64 = test_db.query_one("SELECT count(*) AS count FROM game.epic_arc_missions WHERE character_id=$1", &[&f.character]).await.unwrap().get("count");
            let trigger = format!("mission_history_failure_{}_{}", f.character, action);
            test_db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'standing history write failure'; END $$"), &[]).await.unwrap();
            test_db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.standing_history FOR EACH ROW WHEN (NEW.char_id = {}) EXECUTE FUNCTION game.{trigger}()", f.character), &[]).await.unwrap();
            let args = [Value::Int(action)];
            let call = Call { service: "agentMgr", method: "DoAction", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
            let failed = missions::dispatch(&mut c, &call).await.unwrap();
            test_db.execute(&format!("DROP TRIGGER {trigger} ON game.standing_history"), &[]).await.unwrap();
            test_db.execute(&format!("DROP FUNCTION game.{trigger}()"), &[]).await.unwrap();
            match failed {
                Err(CallError::Internal(error)) => assert_eq!(database_message(&error), Some("standing history write failure")),
                other => panic!("mission {action} history failure was not propagated: {other:?}"),
            }
            assert_eq!(test_db.query_one("SELECT state FROM game.agent_missions WHERE character_id=$1 AND agent_id=$2", &[&f.character, &agent.id]).await.unwrap().get::<_, i32>("state"), observed);
            assert_eq!(test_db.query_one("SELECT count(*) AS count FROM game.epic_arc_missions WHERE character_id=$1", &[&f.character]).await.unwrap().get::<_, i64>("count"), arcs_before);
            assert!(test_db.query("SELECT 1 FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap().is_empty());
            assert!(test_db.query("SELECT 1 FROM game.standing_history WHERE char_id=$1", &[&f.character]).await.unwrap().is_empty());
            assert!(c.outbox.is_empty());
            assert!(missions::dispatch(&mut c, &call).await.unwrap().is_ok());
            assert!(test_db.query("SELECT 1 FROM game.agent_missions WHERE character_id=$1", &[&f.character]).await.unwrap().is_empty());
            // The configured negative rate moves the agent and its corporation toward -10.
            let loss = crate::config::game().standings.decline_loss * 10.0;
            for owner in [agent.id, agent.corp_id] {
                assert_eq!(test_db.query_one("SELECT standing FROM game.standings WHERE char_id=$1 AND from_id=$2", &[&f.character, &owner]).await.unwrap().get::<_, f64>("standing"), loss);
            }
            if quit {
                let current: String = test_db.query_one("SELECT state FROM game.epic_arc_missions WHERE character_id=$1 AND arc_id=$2 AND mission_id=$3", &[&f.character, &arc, &mission_id]).await.unwrap().get("state");
                assert_eq!(current, if retry == mission_id { "unlocked" } else { "quit" });
                if retry != mission_id {
                    let again: String = test_db.query_one("SELECT state FROM game.epic_arc_missions WHERE character_id=$1 AND arc_id=$2 AND mission_id=$3", &[&f.character, &arc, &retry]).await.unwrap().get("state");
                    assert_eq!(again, "unlocked", "quit step's retry unlocked");
                }
            }
            let kinds: Vec<i64> = test_db.query("SELECT event_type_id FROM game.standing_history WHERE char_id=$1", &[&f.character]).await.unwrap().iter().map(|row| row.get("event_type_id")).collect();
            assert!(!kinds.is_empty() && kinds.iter().all(|kind| *kind == event), "mission {action} journaled {kinds:?}");
            // The next round starts from a clean slate for this character.
            test_db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
            test_db.execute("DELETE FROM game.standing_history WHERE char_id=$1", &[&f.character]).await.unwrap();
            test_db.execute("DELETE FROM game.epic_arc_missions WHERE character_id=$1", &[&f.character]).await.unwrap();
            c.outbox.clear();
        }
    }).await;
    for action in [4_i64, 6] {
        let trigger = format!("mission_history_failure_{}_{}", f.character, action);
        db.execute(&format!("DROP TRIGGER IF EXISTS {trigger} ON game.standing_history"), &[]).await.unwrap();
        db.execute(&format!("DROP FUNCTION IF EXISTS game.{trigger}()"), &[]).await.unwrap();
    }
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap();
    db.execute("DELETE FROM game.standing_history WHERE char_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::remaining(&db, f).await.unwrap(), 0);
    assert!(result.is_ok(), "mission end atomicity task failed: {result:?}");
}

