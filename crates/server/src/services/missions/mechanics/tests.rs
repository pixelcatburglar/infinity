//! Eligibility preserves JSON key presence separately from its nullable value.
use super::{eligible_kill, rewards};
use crate::sde::{dungeons::Dungeon, missions::{Mission, JournalMessage}};

#[test]
fn kill_eligibility_uses_key_presence_and_difficulty() {
    let mut m = Mission { mission_id: 1, name_id: None, has_name: true, template: Some("agent.missionTemplatizedContent_BasicKillMission".into()), dungeon_id: Some(1), faction_id: None, cargo_type: None, cargo_qty: None, reward_type: None, reward_quantity: None, reward_lp: None, messages: Vec::new() };
    let d = Dungeon { id: 1, name_id: None, has_name: false, difficulty: Some(2), faction_id: None, archetype: None, gated: false };
    assert!(!eligible_kill(&m, &d, 2));
    m.messages.push(JournalMessage { key: "messages.mission.briefing".into(), mission_id: 1, message_id: None });
    assert!(eligible_kill(&m, &d, 2));
    assert!(!eligible_kill(&m, &d, 1));
    m.has_name = false;
    assert!(!eligible_kill(&m, &d, 2));
}

#[test]
fn rewards_use_level_table_and_clamp_high_levels() {
    crate::tests::init_config();
    let table = &crate::config::game().missions.rewards;
    for level in 1..=5 {
        let reward = &table[(level - 1) as usize];
        assert_eq!(rewards(level), (reward.isk, reward.lp));
    }
    assert_eq!(rewards(i64::MAX), rewards(5));
    assert_eq!(rewards(0), rewards(5));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn real_mission_settles_current_corporation_once_and_rolls_back_failure() {
    use infinity_core::consts::ACCOUNT_CASH;
    use crate::{db::Db, services::tests::{self, fixture}};
    use marshal::Value;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let pilot = fixture::create(&db).await.unwrap();
    let founder = fixture::create(&db).await.unwrap();
    let first = fixture::create_corporation(&db, pilot.character, 60000001).await.unwrap();
    let current = fixture::create_corporation(&db, founder.character, 60000001).await.unwrap();
    let agent = super::super::agents().values().find(|a| a.corp_id > 0 && a.corp_id != infinity_core::consts::CORP_HERALDRY && a.station_id > 0).expect("retail agent required");
    let (client, _peer) = tests::client(db.clone(), pilot.character).await;
    let client = std::sync::Arc::new(tokio::sync::Mutex::new(client));
    let scenario_client = client.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let mut client = scenario_client.lock().await;
        let mission = super::super::Mission {
            agent_id: agent.id, mission_id: 1, name_id: 0, briefing_id: 0, dungeon_id: 0,
            faction_id: 0, state: super::super::STATE_ACCEPTED, system_id: agent.system_id,
            pos: [0.0; 3], reward_isk: 200000, reward_lp: 19, accepted_at: Some(1),
            kind: super::super::Kind::Kill, arc_id: None, dest_agent: None, cargo: None,
        };
        db.execute("UPDATE game.characters SET corporation_id=$2,balance=0 WHERE character_id=$1", &[&pilot.character, &first]).await.unwrap();
        db.execute("UPDATE game.corporations SET tax_rate=0.1,lp_tax_rate=0.1 WHERE corporation_id=$1", &[&first]).await.unwrap();
        db.execute("UPDATE game.corporations SET tax_rate=0.25,lp_tax_rate=0.5 WHERE corporation_id=$1", &[&current]).await.unwrap();
        db.execute("INSERT INTO game.agent_missions(character_id,agent_id,mission_id,dungeon_id,faction_id,state,system_id,x,y,z,reward_isk,reward_lp,offered_at,kind) VALUES ($1,$2,1,0,0,2,$3,0,0,0,200000,19,1,'kill')", &[&pilot.character, &agent.id, &agent.system_id]).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&pilot.character, &current]).await.unwrap();
        db.execute("INSERT INTO game.corp_wallets(corporation_id,account_key,balance) VALUES ($1,$2,'9999999999999999999999999999999999.99'::numeric)", &[&current, &ACCOUNT_CASH]).await.unwrap();
        client.world.system(agent.system_id).await.unwrap();
        client.session.apply(vec![("charid", Value::Int(pilot.character)), ("corpid", Value::Int(first)), ("solarsystemid2", Value::Int(agent.system_id)), ("stationid", Value::Int(agent.station_id))]);
        let cargo_type = fixture::market_type_ids(&client.sde).next().unwrap();
        let first_stack = crate::dao::items::transfers::insert(&db, cargo_type, pilot.character, agent.station_id, infinity_core::consts::FLAG_HANGAR, i64::MAX).await.unwrap();
        crate::dao::items::transfers::insert(&db, cargo_type, pilot.character, agent.station_id, infinity_core::consts::FLAG_HANGAR, i64::MAX).await.unwrap();
        assert!(super::has_goods(&client, agent.station_id, (cargo_type, 10)).await.unwrap());
        let (_, changes) = super::plan_goods(&db, &client.sde, pilot.character, agent.station_id, 0, (cargo_type, 10)).await.unwrap();
        assert_eq!(changes[&first_stack].quantity, Some(i64::MAX - 10));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(pilot.character, tx);
        super::super::sites::force_clear(&client, agent.id, agent.system_id).await.unwrap();
        assert!(super::super::sites::objective_done(&client, &mission).await);
        let before = crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", pilot.character).await.len();
        let corp_before = crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", current).await.len();
        assert!(super::finish(&mut client, &mission, agent, None).await.is_err());
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", pilot.character).await.len(), before);
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", current).await.len(), corp_before);
        assert!(super::super::sites::objective_done(&client, &mission).await);
        assert!(rx.try_recv().is_err());
        assert_eq!(crate::services::wallets::balance(&db, pilot.character).await.unwrap(), crate::isk::IskAmount::ZERO);
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, pilot.character, agent.corp_id).await.unwrap(), None);
        assert!(db.query_opt("SELECT character_id FROM game.agent_missions WHERE character_id=$1 AND agent_id=$2", &[&pilot.character, &agent.id]).await.unwrap().is_some());
        db.execute("UPDATE game.corp_wallets SET balance=0 WHERE corporation_id=$1 AND account_key=$2", &[&current, &ACCOUNT_CASH]).await.unwrap();
        let started = crate::client::filetime_now();
        super::finish(&mut client, &mission, agent, None).await.unwrap();
        let finished = crate::client::filetime_now();
        let facts = crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", pilot.character).await;
        assert_eq!(facts.len(), before + 1);
        assert!(matches!(&facts[before], crate::lua::events::GameEvent::LoyaltyPointsEarned { char_id, issuer_id, amount: 10, reason: crate::lua::events::resources::LoyaltyReason::MissionReward, occurred_at, context }
            if *char_id == pilot.character && *issuer_id == agent.corp_id && (started..=finished).contains(occurred_at)
            && context.corporation_id == Some(current) && context.alliance_id == None && context.faction_id == None
            && context.ship_id == None && context.ship_type_id == None
            && context.system_id == Some(agent.system_id) && context.station_id == Some(agent.station_id)
            && context.origin == crate::lua::events::resources::FactOrigin::Natural));
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", current).await.len(), corp_before);
        assert!(!super::super::sites::objective_done(&client, &mission).await);
        assert_eq!(crate::services::wallets::balance(&db, pilot.character).await.unwrap(), crate::isk::IskAmount::from_cents(15_000_000).unwrap());
        assert_eq!(crate::services::wallets::corp_balance(&db, current, ACCOUNT_CASH).await.unwrap(), crate::isk::IskAmount::from_cents(5_000_000).unwrap());
        assert_eq!(crate::services::wallets::corp_balance(&db, first, ACCOUNT_CASH).await.unwrap(), crate::isk::IskAmount::ZERO);
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, pilot.character, agent.corp_id).await.unwrap(), Some(10));
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, current, agent.corp_id).await.unwrap(), Some(9));
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnAccountChange" && args == &vec![Value::str("cash"), Value::Int(pilot.character), Value::Float(150000.0)]));
        assert!(notices.iter().any(|(event, args)| event == "OnLPChange" && args == &vec![Value::str("lp"), Value::Int(agent.corp_id), Value::Int(0), Value::Int(10)]));
        assert!(super::finish(&mut client, &mission, agent, None).await.is_err());
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", pilot.character).await.len(), before + 1);
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", current).await.len(), corp_before);
        assert_eq!(crate::services::wallets::balance(&db, pilot.character).await.unwrap(), crate::isk::IskAmount::from_cents(15_000_000).unwrap());
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, current, agent.corp_id).await.unwrap(), Some(9));
    }).await;
    {
        let guard = client.lock().await;
        super::super::sites::despawn(&*guard, pilot.character, agent.id);
    }
    db.execute("DELETE FROM game.lp_balances WHERE char_id=ANY($1)", &[&vec![pilot.character,first,current]]).await.unwrap();
    db.execute("DELETE FROM game.standings WHERE char_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=ANY($1)", &[&vec![first,current]]).await.unwrap();
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        fixture::cleanup_progress(tx, &[first, current], &[first, current]).await
    }), |(), ()| async move { Ok(()) }).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&pilot.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&vec![first,current]]).await.unwrap();
    fixture::cleanup(&db, pilot).await.unwrap();
    fixture::cleanup(&db, founder).await.unwrap();
    result.unwrap();
}
