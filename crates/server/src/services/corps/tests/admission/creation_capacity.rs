//! Founding and member capacity snapshots from corporation management dogma.
use super::*;

fn capacity_inputs(sde: &crate::sde::Sde) -> (f64, Vec<(i32, f64)>) {
    let add = sde.dogma.effects_by_name("corporationMemberLimitBonus").next().unwrap().effect_id;
    let multiply = sde.dogma.effects_by_name("corporationManagementSkillBoostCorporationMemberBonus").next().unwrap().effect_id;
    let bonus = sde.dogma.attributes_by_name("corporationMemberBonus").next().unwrap();
    let base = sde.dogma.attributes_by_name("corporationMemberLimit").next().unwrap().default_value.unwrap();
    let skills = sde.types.iter().filter(|ty| ty.effects().iter().any(|e| e.effect_id == add) && ty.effects().iter().any(|e| e.effect_id == multiply))
        .map(|ty| (ty.type_id, ty.attribute(bonus.attribute_id).or(bonus.default_value).unwrap())).collect::<Vec<_>>();
    assert!(!skills.is_empty());
    (base, skills)
}

async fn unique_ticker(db: &Db, sde: &crate::sde::Sde, character: i64) -> String {
    for n in 0..1_000_000 {
        let ticker = format!("{:05X}", (character + n) % 0x100000);
        if sde.npc_corporations.iter().any(|c| c.upper_ticker.as_deref() == Some(&ticker)) { continue; }
        let row = db.query_one("SELECT count(*) AS n FROM game.corporations WHERE upper(ticker)=$1", &[&ticker]).await.unwrap();
        if row.get::<_, i64>("n") == 0 { return ticker; }
    }
    panic!("no unused ticker")
}

async fn cleanup_founder(db: &Db, ceo: fixture::Fixture, others: &[fixture::Fixture]) {
    let rows = db.query("SELECT corporation_id FROM game.corporations WHERE creator_id=$1 OR ceo_id=$1", &[&ceo.character]).await.unwrap();
    if !rows.is_empty() {
        for row in &rows {
            let corp: i64 = row.get("corporation_id");
            db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
            db.execute("DELETE FROM game.mail_status WHERE message_id IN (SELECT message_id FROM game.mail_messages WHERE sender_id=$1)", &[&corp]).await.unwrap();
            db.execute("DELETE FROM game.mail_messages WHERE sender_id=$1", &[&corp]).await.unwrap();
            db.execute("DELETE FROM game.corp_applications WHERE corporation_id=$1", &[&corp]).await.unwrap();
            db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
        }
        let mut people = vec![ceo];
        people.extend_from_slice(others);
        for row in &rows[1..] {
            let corp: i64 = row.get("corporation_id");
            db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
        }
        cleanup(db, rows[0].get("corporation_id"), &people).await;
    } else {
        fixture::cleanup(db, ceo).await.unwrap();
        for person in others { fixture::cleanup(db, *person).await.unwrap(); }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn creation_saves_skill_capacity_and_charges_startup_cost() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(1))]);
        let old_corp = corp_of(&test_db, ceo.character).await;
        let (founder_tx, mut founder_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(ceo.character, founder_tx);
        let (base, skills) = capacity_inputs(&client.sde);
        let skill = super::super::super::SKILL_CORPORATION_MANAGEMENT;
        let bonus = skills.iter().find(|(id, _)| *id == skill).unwrap().1;
        let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
        test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&ceo.character, &skill, &level]).await.unwrap();
        let starting = super::super::super::STARTUP_COST * 2;
        test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&ceo.character, &starting.to_string()]).await.unwrap();
        let ticker = unique_ticker(&test_db, &client.sde, ceo.character).await;
        let lp_tax = 0.25;
        let args = [Value::unicode(format!("Fixture Corporation {}", ceo.character)), Value::unicode(ticker), Value::unicode("description"), Value::unicode(""), Value::Float(0.0), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::Bool(false), Value::Bool(true), Value::Float(lp_tax)];
        let corp = rpc(&mut client, "AddCorporation", &args, 1).await.unwrap().as_int().unwrap();
        let events: Vec<_> = std::iter::from_fn(|| founder_rx.try_recv().ok()).collect();
        let session = events.iter().position(|e| e.0 == "__session").unwrap();
        assert!(events[session].1.contains(&Value::tuple([Value::str("corpid"), Value::Int(corp)])));
        assert!(events[session].1.contains(&Value::tuple([Value::str("corpAccountKey"), Value::Int(infinity_core::consts::ACCOUNT_CASH as i64)])));
        let members: Vec<_> = events.iter().enumerate().filter(|(_, e)| e.0 == "OnCorporationMemberChanged").collect();
        assert_eq!(members.len(), 1);
        assert!(session < members[0].0);
        assert_eq!(members[0].1.1, vec![Value::Int(corp), Value::Int(ceo.character), Value::dict([("corporationID", Value::tuple([Value::Int(old_corp), Value::Int(corp)]))])]);
        let counts: Vec<_> = events.iter().enumerate().filter(|(_, e)| e.0 == "OnCorporationChanged").collect();
        assert_eq!(counts.len(), 1);
        assert!(session < counts[0].0);
        assert_eq!(counts[0].1.1, vec![Value::Int(corp), Value::dict([("memberCount", Value::tuple([Value::Int(0), Value::Int(1)]))])]);
        let row = test_db.query_one("SELECT member_limit,lp_tax_rate,is_recruiting,friendly_fire FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
        assert_eq!(row.get::<_, Option<i64>>("member_limit"), Some((base + bonus * f64::from(level)) as i64));
        assert_eq!(row.get::<_, f64>("lp_tax_rate"), lp_tax);
        assert!(!row.get::<_, bool>("is_recruiting"));
        assert!(row.get::<_, bool>("friendly_fire"));
        assert_eq!(corp_of(&test_db, ceo.character).await, corp);
        assert_eq!(crate::services::wallets::balance(&test_db, ceo.character).await.unwrap(), crate::isk::IskAmount::from_whole_isk(starting).unwrap().checked_sub(crate::isk::IskAmount::from_whole_isk(super::super::super::STARTUP_COST).unwrap()).unwrap());
    }).await;
    cleanup_founder(&db, ceo, &[]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn creation_refuses_nonfinite_tax_rates_without_persisting() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for field in [4, 14] {
        for rate in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let ceo = fixture::create(&db).await.unwrap();
            let test_db = db.clone();
            let result = tokio::spawn(async move {
                let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
                client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(1))]);
                let skill = super::super::super::SKILL_CORPORATION_MANAGEMENT;
                let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
                test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&ceo.character, &skill, &level]).await.unwrap();
                let starting = super::super::super::STARTUP_COST * 2;
                test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&ceo.character, &starting.to_string()]).await.unwrap();
                let ticker = unique_ticker(&test_db, &client.sde, ceo.character).await;
                let mut args = [Value::unicode(format!("Fixture Tax {}", ceo.character)), Value::unicode(ticker), Value::unicode("description"), Value::unicode(""), Value::Float(0.0), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::Bool(false), Value::Bool(false), Value::Float(0.0)];
                args[field] = Value::Float(rate);
                let response = rpc(&mut client, "AddCorporation", &args, 1).await;
                let count: i64 = test_db.query_one("SELECT count(*) AS n FROM game.corporations WHERE creator_id=$1 OR ceo_id=$1", &[&ceo.character]).await.unwrap().get("n");
                assert_eq!(count, 0, "field {field}, rate {rate:?} persisted a corporation");
                assert!(matches!(response, Err(crate::services::CallError::User { .. })), "field {field}, rate {rate:?}: {response:?}");
                assert_eq!(crate::services::wallets::balance(&test_db, ceo.character).await.unwrap(), crate::isk::IskAmount::from_whole_isk(starting).unwrap());
            }).await;
            cleanup_founder(&db, ceo, &[]).await;
            result.unwrap();
        }
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn creation_clamps_finite_tax_rates() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for (tax, lp_tax, expected_tax, expected_lp_tax) in [(-0.5, 1.5, 0.0, 1.0), (1.5, -0.5, 1.0, 0.0)] {
        let ceo = fixture::create(&db).await.unwrap();
        let test_db = db.clone();
        let result = tokio::spawn(async move {
            let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
            client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(1))]);
            let skill = super::super::super::SKILL_CORPORATION_MANAGEMENT;
            let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
            test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&ceo.character, &skill, &level]).await.unwrap();
            let starting = super::super::super::STARTUP_COST * 2;
            test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&ceo.character, &starting.to_string()]).await.unwrap();
            let ticker = unique_ticker(&test_db, &client.sde, ceo.character).await;
            let args = [Value::unicode(format!("Fixture Tax Clamp {}", ceo.character)), Value::unicode(ticker), Value::unicode("description"), Value::unicode(""), Value::Float(tax), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::Bool(false), Value::Bool(false), Value::Float(lp_tax)];
            let corp = rpc(&mut client, "AddCorporation", &args, 1).await.unwrap().as_int().unwrap();
            let row = test_db.query_one("SELECT tax_rate,lp_tax_rate FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
            assert_eq!(row.get::<_, f64>("tax_rate"), expected_tax);
            assert_eq!(row.get::<_, f64>("lp_tax_rate"), expected_lp_tax);
        }).await;
        cleanup_founder(&db, ceo, &[]).await;
        result.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn creation_persists_boolean_and_integer_recruiting_and_lp_tax() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for (recruiting, expected) in [(Value::Bool(false), false), (Value::Bool(true), true), (Value::Int(0), false), (Value::Int(1), true)] {
        let ceo = fixture::create(&db).await.unwrap();
        let test_db = db.clone();
        let result = tokio::spawn(async move {
            let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
            client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(1))]);
            let skill = super::super::super::SKILL_CORPORATION_MANAGEMENT;
            let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
            test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&ceo.character, &skill, &level]).await.unwrap();
            let starting = super::super::super::STARTUP_COST * 2;
            test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&ceo.character, &starting.to_string()]).await.unwrap();
            let ticker = unique_ticker(&test_db, &client.sde, ceo.character).await;
            let lp_tax = 0.25;
            let args = [Value::unicode(format!("Fixture Recruiting {}", ceo.character)), Value::unicode(ticker), Value::unicode("description"), Value::unicode(""), Value::Float(0.0), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, recruiting, Value::Bool(false), Value::Float(lp_tax)];
            let corp = rpc(&mut client, "AddCorporation", &args, 1).await.unwrap().as_int().unwrap();
            let row = test_db.query_one("SELECT is_recruiting,lp_tax_rate FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
            assert_eq!(row.get::<_, bool>("is_recruiting"), expected);
            assert_eq!(row.get::<_, f64>("lp_tax_rate"), lp_tax);
        }).await;
        cleanup_founder(&db, ceo, &[]).await;
        result.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn concurrent_foundings_by_same_character_charge_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut first, _first_peer) = tests::client(test_db.clone(), ceo.character).await;
        let (mut second, _second_peer) = tests::client(test_db.clone(), ceo.character).await;
        for client in [&mut first, &mut second] {
            client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(1))]);
        }
        let skill = super::super::super::SKILL_CORPORATION_MANAGEMENT;
        let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
        test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&ceo.character, &skill, &level]).await.unwrap();
        let starting = super::super::super::STARTUP_COST * 3;
        test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&ceo.character, &starting.to_string()]).await.unwrap();
        let ticker1 = unique_ticker(&test_db, &first.sde, ceo.character).await;
        let mut seed = ceo.character + 1;
        let ticker2 = loop {
            let ticker = unique_ticker(&test_db, &first.sde, seed).await;
            if ticker != ticker1 { break ticker; }
            seed += 1;
        };
        let args = |suffix, ticker, recruiting| [Value::unicode(format!("Fixture Race {} {}", ceo.character, suffix)), Value::unicode(ticker), Value::unicode("description"), Value::unicode(""), Value::Float(0.0), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, recruiting, Value::Bool(false), Value::Float(0.0)];
        let args1 = args("A", ticker1, Value::Bool(true));
        let args2 = args("B", ticker2, Value::Bool(true));
        // Regardless of scheduling, one founder may create only one corporation.
        let (one, two) = tokio::join!(rpc(&mut first, "AddCorporation", &args1, 1), rpc(&mut second, "AddCorporation", &args2, 1));
        let (winner, loser) = match (one, two) {
            (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
            other => panic!("expected one success and one refusal, got {other:?}"),
        };
        match loser {
            crate::services::CallError::User { msg, dict } => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert!(dict == vec![("notify".to_owned(), Value::unicode(infinity_core::messages::CORP_APPLICATION_ALREADY_PROCESSED))]
                    || dict == vec![("notify".to_owned(), Value::unicode(infinity_core::messages::ALREADY_CORP_CEO_MUST_RESIGN_OR_CLOSE))]);
            }
            other => panic!("expected UserError, got {other:?}"),
        }
        let corp = winner.as_int().unwrap();
        let rows = test_db.query("SELECT corporation_id,ceo_id,is_recruiting,deleted FROM game.corporations WHERE creator_id=$1 OR ceo_id=$1", &[&ceo.character]).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<_, i64>("corporation_id"), corp);
        assert_eq!(rows[0].get::<_, Option<i64>>("ceo_id"), Some(ceo.character));
        assert!(!rows[0].get::<_, bool>("deleted"));
        assert!(rows[0].get::<_, bool>("is_recruiting"));
        assert_eq!(corp_of(&test_db, ceo.character).await, corp);
        assert_eq!(crate::services::wallets::balance(&test_db, ceo.character).await.unwrap(), crate::isk::IskAmount::from_whole_isk(starting).unwrap().checked_sub(crate::isk::IskAmount::from_whole_isk(super::super::super::STARTUP_COST).unwrap()).unwrap());
        let count: i64 = test_db.query_one("SELECT count(*) AS n FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2", &[&ceo.character, &corp]).await.unwrap().get("n");
        assert_eq!(count, 1);
        let error = rpc(&mut second, "AddCorporation", &args2, 1).await.unwrap_err();
        match error {
            crate::services::CallError::User { msg, dict } => {
                assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
                assert_eq!(dict, vec![("notify".to_owned(), Value::unicode(infinity_core::messages::ALREADY_CORP_CEO_MUST_RESIGN_OR_CLOSE))]);
            }
            other => panic!("expected UserError, got {other:?}"),
        }
        assert_eq!(crate::services::wallets::balance(&test_db, ceo.character).await.unwrap(), crate::isk::IskAmount::from_whole_isk(starting).unwrap().checked_sub(crate::isk::IskAmount::from_whole_isk(super::super::super::STARTUP_COST).unwrap()).unwrap());
    }).await;
    cleanup_founder(&db, ceo, &[]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn gm_zero_capacity_keeps_ceo_and_refuses_join_without_changing_npc_projection() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        client.session.apply(vec![("corpid", Value::Int(1)), ("stationid", Value::Int(1))]);
        let npc = client.sde.npc_corporations.iter().find(|c| c.corporation_id != 1).unwrap();
        let npc_id = npc.corporation_id;
        let npc_limit = npc.member_limit.unwrap_or(infinity_core::consts::CORPORATION_UNLIMITED_MEMBER_LIMIT);
        let kwargs = Value::Dict(vec![]);
        let args = [Value::Int(npc_id)];
        let call = Call { service: "corpmgr", method: "GetPublicInfo", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        let before = crate::services::universe::dispatch(&mut client, &call).await.unwrap().unwrap();
        let Value::Instance(_, fields) = &before else { panic!("expected corporation row") };
        assert_eq!(fields.get("line").unwrap().items().unwrap()[19], Value::Int(npc_limit));
        let ticker = unique_ticker(&test_db, &client.sde, ceo.character).await;
        let corp = super::super::super::corp_registry::gm_found(&mut client, &format!("Fixture GM {}", ceo.character), &ticker).await.unwrap();
        let row = test_db.query_one("SELECT ceo_id,member_limit FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
        assert_eq!(row.get::<_, Option<i64>>("ceo_id"), Some(ceo.character));
        assert_eq!(row.get::<_, Option<i64>>("member_limit"), Some(0));
        assert_eq!(corp_of(&test_db, ceo.character).await, corp);
        let id = application(&test_db, corp, applicant.character, 8).await;
        client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
        assert!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), 1).await.is_err());
        assert_eq!(status(&test_db, id).await, 8);
        assert_eq!(corp_of(&test_db, applicant.character).await, 1);
        assert_eq!(crate::services::universe::dispatch(&mut client, &call).await.unwrap().unwrap(), before);
    }).await;
    cleanup_founder(&db, ceo, &[applicant]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn all_management_skill_level_combinations_use_sde_bonuses() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        client.session.apply(vec![("corpid", Value::Int(corp))]);
        let (base, skills) = capacity_inputs(&client.sde);
        let combinations = 6_usize.pow(u32::try_from(skills.len()).unwrap());
        for mut combination in 0..combinations {
            let mut expected = base;
            for &(skill, bonus) in &skills {
                let level = (combination % 6) as i32;
                combination /= 6;
                expected += bonus * f64::from(level);
                test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=$3", &[&ceo.character, &skill, &level]).await.unwrap();
            }
            assert_eq!(rpc(&mut client, "UpdateCorporationAbilities", &[], corp).await.unwrap(), Value::Bool(true));
            let saved: Option<i64> = test_db.query_one("SELECT member_limit FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get("member_limit");
            assert_eq!(saved, Some(expected as i64));
        }
    }).await;
    cleanup(&db, corp, &[ceo]).await;
    result.unwrap();
}

mod exit_boundaries;
