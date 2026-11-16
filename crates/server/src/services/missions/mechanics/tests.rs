//! Offer-quote and completion-gain coverage plus the existing settlement atomicity.
use super::*;
use crate::sde::{dungeons::Dungeon, missions::{Mission, JournalMessage}};
use crate::services::tests::{client, fixture};
use infinity_core::consts::{SKILL_SOCIAL, dogma::{ATTR_SOCIAL_BONUS, ATTR_SOCIAL_MUTATOR}};

/// SDE invTypes: Negotiation skill.
const SKILL_NEGOTIATION: i32 = 3356;
/// SDE invTypes: Distribution, Mining and Security Connections.
const SKILL_DISTRIBUTION_CONNECTIONS: i32 = 3894;
const SKILL_MINING_CONNECTIONS: i32 = 3893;
const SKILL_SECURITY_CONNECTIONS: i32 = 3895;
/// SDE invTypes: Basic Glamourex Booster (no injection deadline or prerequisites).
const BOOSTER_GLAMOUREX: i32 = 64724;
/// SDE dgmEffectIDs: Social booster binding effect.
const EFFECT_SOCIAL_MUTATOR: i32 = 407;

async fn train(db: &crate::db::Db, sde: &crate::sde::Sde, character: i64, type_id: i32, level: i32) {
    let points = crate::services::skills::formulas::sp_for_level(crate::services::skills::training::skill_type(sde, type_id).rank, i64::from(level));
    crate::dao::skills::training::set_level(db, character, type_id, level, points).await.unwrap();
}

async fn offer_and_accept(db: &crate::db::Db, c: &Client, agent: &super::super::Agent, char_id: i64, fit: &Fit, at: i64) -> super::super::Mission {
    super::offer(c, agent, fit, at).await.unwrap();
    let mission = super::mission_with(c, agent.id).await.unwrap().unwrap();
    assert_eq!(crate::dao::missions::accept_offered(db, char_id, agent.id, mission.mission_id, super::super::STATE_OFFERED, super::super::STATE_ACCEPTED, at).await.unwrap(), 1);
    super::mission_with(c, agent.id).await.unwrap().unwrap()
}

#[test]
fn reward_quote_floors_once_and_rejects_out_of_range() {
    assert_eq!((quote_isk(1.0, 123_456).ok(), quote_isk(1.1, 199_999).ok(), quote_lp(0, 450).ok(), quote_lp(3, 451).ok()), (Some(123_456), Some(219_998), Some(450), Some(586)));
    for result in [quote_isk(f64::NAN, 1), quote_isk(-1.0, 1), quote_isk(1.0, -1), quote_isk(2.0, i64::MAX), quote_lp(-1, 1), quote_lp(1, -1), quote_lp(5, i64::MAX)] {
        assert!(matches!(result, Err(CallError::Internal(_))));
    }
}

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
    let agent = super::super::agents().values().find(|a| a.type_id == super::super::AGENT_TYPE_BASIC && a.level >= 2 && a.division_id == 24 && a.corp_id > 0 && a.corp_id != infinity_core::consts::CORP_HERALDRY && a.station_id > 0).expect("retail agent required");
    let (client, _peer) = tests::client(db.clone(), pilot.character).await;
    let client = std::sync::Arc::new(tokio::sync::Mutex::new(client));
    let scenario_client = client.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let mut client = scenario_client.lock().await;
        let at = crate::client::filetime_now();
        train(&db, &client.sde, pilot.character, SKILL_NEGOTIATION, 2).await;
        train(&db, &client.sde, pilot.character, SKILL_SECURITY_CONNECTIONS, 2).await;
        let fit = Fit::load_character_at(&db, client.sde.clone(), pilot.character, at).await.unwrap();
        db.execute("UPDATE game.characters SET corporation_id=$2,balance=0 WHERE character_id=$1", &[&pilot.character, &first]).await.unwrap();
        db.execute("UPDATE game.corporations SET tax_rate=0.1,lp_tax_rate=0.1 WHERE corporation_id=$1", &[&first]).await.unwrap();
        db.execute("UPDATE game.corporations SET tax_rate=0.25,lp_tax_rate=0.5 WHERE corporation_id=$1", &[&current]).await.unwrap();
        let mission = offer_and_accept(&db, &client, agent, pilot.character, &fit, at).await;
        let (base_isk, base_lp) = super::rewards(agent.level);
        assert_eq!((mission.reward_isk, mission.reward_lp), ((base_isk as f64 * (1.0 + 5.0 * 2.0 / 100.0)).floor() as i64, base_lp * (10 + 2) / 10));
        let gross_isk = mission.reward_isk;
        let gross_lp = mission.reward_lp;
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
        let corp_lp = (gross_lp as f64 * 0.5).floor() as i64;
        let pilot_lp = gross_lp - corp_lp;
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
        assert!(matches!(&facts[before], crate::lua::events::GameEvent::LoyaltyPointsEarned { char_id, issuer_id, amount, reason: crate::lua::events::resources::LoyaltyReason::MissionReward, occurred_at, context }
            if *char_id == pilot.character && *issuer_id == agent.corp_id && *amount == pilot_lp && (started..=finished).contains(occurred_at)
            && context.corporation_id == Some(current) && context.alliance_id == None && context.faction_id == None
            && context.ship_id == None && context.ship_type_id == None
            && context.system_id == Some(agent.system_id) && context.station_id == Some(agent.station_id)
            && context.origin == crate::lua::events::resources::FactOrigin::Natural));
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", current).await.len(), corp_before);
        assert!(!super::super::sites::objective_done(&client, &mission).await);
        assert_eq!(crate::services::wallets::balance(&db, pilot.character).await.unwrap(), crate::isk::IskAmount::from_isk(gross_isk as f64 * 0.75).unwrap());
        assert_eq!(crate::services::wallets::corp_balance(&db, current, ACCOUNT_CASH).await.unwrap(), crate::isk::IskAmount::from_isk(gross_isk as f64 * 0.25).unwrap());
        assert_eq!(crate::services::wallets::corp_balance(&db, first, ACCOUNT_CASH).await.unwrap(), crate::isk::IskAmount::ZERO);
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, pilot.character, agent.corp_id).await.unwrap(), Some(pilot_lp));
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, current, agent.corp_id).await.unwrap(), Some(corp_lp));
        let notices: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(notices.iter().any(|(event, args)| event == "OnAccountChange" && args == &vec![Value::str("cash"), Value::Int(pilot.character), Value::Float(gross_isk as f64 * 0.75)]));
        assert!(notices.iter().any(|(event, args)| event == "OnLPChange" && args == &vec![Value::str("lp"), Value::Int(agent.corp_id), Value::Int(0), Value::Int(pilot_lp)]));
        assert!(super::finish(&mut client, &mission, agent, None).await.is_err());
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", pilot.character).await.len(), before + 1);
        assert_eq!(crate::db::outbox::tests::events(&db, "LoyaltyPointsEarned", current).await.len(), corp_before);
        assert_eq!(crate::services::wallets::balance(&db, pilot.character).await.unwrap(), crate::isk::IskAmount::from_isk(gross_isk as f64 * 0.75).unwrap());
        assert_eq!(crate::dao::wallets::loyalty::balance(&db, current, agent.corp_id).await.unwrap(), Some(corp_lp));
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

fn division_agent(division: i64) -> &'static super::super::Agent {
    super::super::agents().values().find(|a| a.type_id == super::super::AGENT_TYPE_BASIC && a.division_id == division && a.corp_id != 0 && a.station_id > 0).expect("retail basic agent required")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn skill_quotes_freeze_and_social_gain_scales_completion() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = client(test_db.clone(), f.character).await;
        let data = c.sde.types.get(BOOSTER_GLAMOUREX).expect("social booster in the SDE");
        assert_eq!((data.published, data.group_id, data.attribute(ATTR_SOCIAL_MUTATOR), data.attribute(infinity_core::consts::dogma::ATTR_BOOSTER_DURATION), data.attribute(infinity_core::consts::dogma::ATTR_BOOSTER_LAST_INJECTION_DATETIME), data.effects().iter().any(|e| e.effect_id == EFFECT_SOCIAL_MUTATOR)),
            (Some(true), Some(infinity_core::consts::GROUP_BOOSTER), Some(15.0), Some(86_400_000.0), None, true));
        let mut binding = data.modifiers(&c.sde.dogma, ATTR_SOCIAL_BONUS);
        let (_, modifier) = binding.next().expect("social bonus modifier");
        assert_eq!((modifier.func.as_str(), modifier.domain.as_str(), modifier.op, modifier.modified, modifier.modifying), ("ItemModifier", "charID", crate::services::fit::OP_POST_PERCENT, ATTR_SOCIAL_BONUS, ATTR_SOCIAL_MUTATOR));
        drop(binding);
        let agent = division_agent(24);
        train(&test_db, &c.sde, f.character, SKILL_SOCIAL, 0).await;
        let at = crate::client::filetime_now();
        let fit = Fit::load_character_at(&test_db, c.sde.clone(), f.character, at).await.unwrap();
        assert_eq!(crate::services::standings::mutation::mission_gain_multiplier(&fit), 1.0);
        let mission = offer_and_accept(&test_db, &c, agent, f.character, &fit, at).await;
        train(&test_db, &c.sde, f.character, SKILL_SOCIAL, 3).await;
        test_db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&f.character, &agent.station_id]).await.unwrap();
        c.session.apply(vec![("stationid", marshal::Value::Int(agent.station_id))]);
        let booster_item = crate::dao::items::transfers::insert(&test_db, BOOSTER_GLAMOUREX, f.character, agent.station_id, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        crate::services::skills::brain_items::use_booster(&mut c, &[marshal::Value::Int(booster_item), marshal::Value::Int(agent.station_id)]).await.unwrap();
        c.world.system(agent.system_id).await.unwrap();
        super::super::sites::force_clear(&c, agent.id, agent.system_id).await.unwrap();
        assert!(super::super::sites::objective_done(&c, &mission).await);
        let trigger = format!("social_gain_failure_{}", f.character);
        test_db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'standing write failure'; END $$"), &[]).await.unwrap();
        test_db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.standing_history FOR EACH ROW WHEN (NEW.char_id = {}) EXECUTE FUNCTION game.{trigger}()", f.character), &[]).await.unwrap();
        assert!(super::finish(&mut c, &mission, agent, None).await.is_err());
        assert!(test_db.query("SELECT 1 FROM game.standings WHERE char_id=$1", &[&f.character]).await.unwrap().is_empty());
        assert!(test_db.query("SELECT 1 FROM game.standing_history WHERE char_id=$1", &[&f.character]).await.unwrap().is_empty());
        test_db.execute(&format!("DROP TRIGGER {trigger} ON game.standing_history"), &[]).await.unwrap();
        test_db.execute(&format!("DROP FUNCTION game.{trigger}()"), &[]).await.unwrap();
        super::finish(&mut c, &mission, agent, None).await.unwrap();
        let policy = &crate::config::game().standings;
        let (gain, limit) = (policy.mission_corp_gain * agent.level.max(1) as f64, super::super::standings::STANDING_LIMIT);
        let owners = [(agent.id, gain * policy.mission_agent_gain_multiplier), (agent.corp_id, gain), (agent.faction_id, gain / policy.mission_faction_gain_divisor)];
        for (owner, base_rate) in owners {
            let expected = base_rate * (1.15 * 1.15) * limit;
            assert_eq!(crate::services::standings::standing(&test_db, f.character, owner).await.unwrap(), expected);
            let h = test_db.query_one("SELECT old_standing,new_standing FROM game.standing_history WHERE char_id=$1 AND from_id=$2", &[&f.character, &owner]).await.unwrap();
            assert_eq!((h.get::<_, f64>("old_standing"), h.get::<_, f64>("new_standing")), (0.0, expected));
        }
        // Second completion with the booster expired: only the Social III bonus applies.
        let mission = offer_and_accept(&test_db, &c, agent, f.character, &fit, at).await;
        test_db.execute("UPDATE game.items SET brain_expires_at=$2 WHERE item_id=$1", &[&booster_item, &(crate::client::filetime_now() - 1)]).await.unwrap();
        c.world.system(agent.system_id).await.unwrap();
        super::super::sites::force_clear(&c, agent.id, agent.system_id).await.unwrap();
        assert!(super::super::sites::objective_done(&c, &mission).await);
        super::finish(&mut c, &mission, agent, None).await.unwrap();
        for (owner, base_rate) in owners {
            let old = base_rate * (1.15 * 1.15) * limit;
            assert_eq!(crate::services::standings::standing(&test_db, f.character, owner).await.unwrap(), old + (limit - old) * (base_rate * 1.15));
        }
        for (skill, level) in [(SKILL_NEGOTIATION, 2), (SKILL_DISTRIBUTION_CONNECTIONS, 1), (SKILL_MINING_CONNECTIONS, 2), (SKILL_SECURITY_CONNECTIONS, 3)] {
            train(&test_db, &c.sde, f.character, skill, level).await;
        }
        let fit = Fit::load_character_at(&test_db, c.sde.clone(), f.character, at).await.unwrap();
        let isk_factor = 1.0 + 5.0 * 2.0 / 100.0;
        for (division, connections) in [(22_i64, 1_i64), (23, 2), (24, 3)] {
            let quoted = division_agent(division);
            super::offer(&c, quoted, &fit, at).await.unwrap();
            let m = super::mission_with(&c, quoted.id).await.unwrap().unwrap();
            let (isk, lp) = super::rewards(quoted.level);
            assert_eq!((m.reward_isk, m.reward_lp, m.kind), ((isk as f64 * isk_factor).floor() as i64, lp * (10 + connections) / 10, super::super::Kind::Kill));
        }
        assert_eq!(reward_quote(&fit, 18, (100, 30)).ok(), Some((110, 30)));
        let (mission_id, arc_agent) = {
            let step = c.sde.missions.steps().find(|s| super::super::agent(s.agent).is_some_and(|a| a.type_id != super::super::AGENT_TYPE_BASIC)
                && c.sde.missions.get(s.mission_id).is_some_and(|m| m.reward_type == Some(super::super::TYPE_CREDITS as i32) && m.reward_quantity.is_some_and(|q| q > 0))).expect("credit arc step at a non-basic retail agent");
            (step.mission_id, super::super::agent(step.agent).unwrap())
        };
        super::offer_arc(&c, arc_agent, mission_id, &fit, at).await.unwrap();
        let data = c.sde.missions.get(mission_id).unwrap();
        let arc_level = DIVISION_LP_SKILL.iter().find(|(d, _)| *d == arc_agent.division_id).map_or(0, |&(_, s)| [(SKILL_DISTRIBUTION_CONNECTIONS, 1), (SKILL_MINING_CONNECTIONS, 2), (SKILL_SECURITY_CONNECTIONS, 3)].iter().find(|(t, _)| *t == s).map_or(0, |&(_, l)| l));
        let expected_arc = ((data.reward_quantity.unwrap() as f64 * isk_factor).floor() as i64, data.reward_lp.unwrap_or(0) * (10 + arc_level) / 10);
        assert_eq!(super::mission_with(&c, arc_agent.id).await.unwrap().map(|m| (m.mission_id, m.reward_isk, m.reward_lp)), Some((mission_id, expected_arc.0, expected_arc.1)));
        // The client sees the frozen integers; skills raised afterwards reprice neither offer.
        let m = super::mission_with(&c, agent.id).await.unwrap().unwrap();
        let (kwargs, bind) = (marshal::Value::None, marshal::Value::Int(agent.id));
        let call = crate::services::Call { service: "agentMgr", method: "GetMissionObjectiveInfo", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&bind) };
        let marshal::Value::Dict(info) = super::super::dispatch(&mut c, &call).await.unwrap().unwrap() else { panic!("objective info must be a dict") };
        let field = |key: &str| info.iter().find(|(k, _)| *k == marshal::Value::str(key)).map(|(_, v)| v);
        assert_eq!(field("loyaltyPoints"), Some(&marshal::Value::Int(m.reward_lp)));
        assert!(matches!(field("normalRewards"), Some(marshal::Value::List(rows))
            if matches!(rows.first(), Some(marshal::Value::Tuple(t)) if t[0] == marshal::Value::Int(super::super::TYPE_CREDITS) && t[1] == marshal::Value::Int(m.reward_isk))));
        for skill in [SKILL_NEGOTIATION, SKILL_SECURITY_CONNECTIONS] {
            train(&test_db, &c.sde, f.character, skill, 5).await;
        }
        let fit = Fit::load_character_at(&test_db, c.sde.clone(), f.character, at).await.unwrap();
        super::offer_arc(&c, arc_agent, mission_id, &fit, at).await.unwrap();
        assert_eq!(super::mission_with(&c, arc_agent.id).await.unwrap().map(|m| (m.reward_isk, m.reward_lp)), Some(expected_arc));
        let m = offer_and_accept(&test_db, &c, agent, f.character, &fit, at).await;
        let (isk_before, lp_before) = (fixture::isk_cents(crate::services::wallets::balance(&test_db, f.character).await.unwrap()), crate::dao::wallets::loyalty::balance(&test_db, f.character, agent.corp_id).await.unwrap().unwrap_or(0));
        c.world.system(agent.system_id).await.unwrap();
        super::super::sites::force_clear(&c, agent.id, agent.system_id).await.unwrap();
        assert!(super::super::sites::objective_done(&c, &m).await);
        super::finish(&mut c, &m, agent, None).await.unwrap();
        assert_eq!(fixture::isk_cents(crate::services::wallets::balance(&test_db, f.character).await.unwrap()) - isk_before, i128::from(m.reward_isk) * 100);
        assert_eq!(crate::dao::wallets::loyalty::balance(&test_db, f.character, agent.corp_id).await.unwrap(), Some(lp_before + m.reward_lp));
    }).await;
    let trigger = format!("social_gain_failure_{}", f.character);
    db.execute(&format!("DROP TRIGGER IF EXISTS {trigger} ON game.standing_history"), &[]).await.unwrap();
    db.execute(&format!("DROP FUNCTION IF EXISTS game.{trigger}()"), &[]).await.unwrap();
    for table in ["standings", "standing_history", "lp_balances"] {
        db.execute(&format!("DELETE FROM game.{table} WHERE char_id=$1"), &[&f.character]).await.unwrap();
    }
    crate::scheduler::cancel(&db, &format!("skills.complete:{}", f.character)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    result.unwrap();
}
