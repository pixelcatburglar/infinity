//! Player-facing corporation application state transitions.
use crate::{client::Client, consts::ROLE_DIRECTOR, db::Db, services::{Call, CallResult, tests::{self, fixture}}};
mod admission_events;
mod creation_capacity;
mod locking;
use marshal::Value;

async fn rpc(client: &mut Client, method: &str, args: &[Value], bind: i64) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let binding = Value::Int(bind);
    let call = Call { service: "corpRegistry", method, args, kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
    super::super::dispatch(client, &call).await.expect("corporation method routed")
}

fn offer(id: i64, character: i64, corp: i64, status: i64) -> Vec<Value> {
    vec![Value::Int(id), Value::Int(character), Value::Int(corp), Value::unicode("申请"), Value::Int(status), Value::unicode("response"), Value::None]
}

async fn application(db: &Db, corp: i64, character: i64, status: i32) -> i64 {
    db.query_one("INSERT INTO game.corp_applications (corporation_id,character_id,application_text,status,created) VALUES ($1,$2,'申请',$3,123) RETURNING application_id", &[&corp, &character, &status]).await.unwrap().get("application_id")
}

async fn status(db: &Db, id: i64) -> i32 {
    db.query_one("SELECT status FROM game.corp_applications WHERE application_id=$1", &[&id]).await.unwrap().get("status")
}

async fn corp_of(db: &Db, character: i64) -> i64 {
    db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap().get("corporation_id")
}

async fn cleanup(db: &Db, corp: i64, people: &[fixture::Fixture]) {
    let ids: Vec<i64> = people.iter().map(|p| p.character).collect();
    db.execute("DELETE FROM game.mail_status WHERE message_id IN (SELECT message_id FROM game.mail_messages WHERE sender_id=$1)", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.mail_messages WHERE sender_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_applications WHERE corporation_id=$1 OR character_id=ANY($2)", &[&corp, &ids]).await.unwrap();
    db.execute("DELETE FROM game.employment_history WHERE char_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1, corp_roles=0 WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for person in people { fixture::cleanup(db, *person).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn application_transition_authority_and_terminal_states() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    fixture::assign_corporation(&db, manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), manager.character).await;
        client.session.apply(vec![("corpid", Value::Int(corp))]);
        for (from, to, by_manager) in [(0, 4, true), (6, 3, false), (8, 3, false), (0, 7, false), (6, 7, false), (8, 7, false)] {
            let id = application(&test_db, corp, applicant.character, from).await;
            let actor = if by_manager { manager.character } else { applicant.character };
            let bound = corp_of(&test_db, actor).await;
            client.session.apply(vec![("charid", Value::Int(actor)), ("corpid", Value::Int(bound))]);
            rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, to), bound).await.unwrap();
            assert_eq!(status(&test_db, id).await, to as i32);
            assert_eq!(corp_of(&test_db, applicant.character).await, 1);
        }
        for from in [1, 2, 3, 4, 5, 7] {
            let id = application(&test_db, corp, applicant.character, from).await;
            client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
            assert!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), 1).await.is_err());
            assert_eq!(status(&test_db, id).await, from);
        }
        let id = application(&test_db, corp, applicant.character, 0).await;
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        for args in [offer(id + 10_000_000, applicant.character, corp, 6), offer(id, outsider.character, corp, 6), offer(id, applicant.character, corp + 10_000_000, 6)] {
            assert!(rpc(&mut client, "UpdateApplicationOffer", &args, corp).await.is_err());
            assert_eq!(status(&test_db, id).await, 0);
        }
        client.session.apply(vec![("charid", Value::Int(outsider.character)), ("corpid", Value::Int(1))]);
        assert!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 7), 1).await.is_err());
        assert_eq!(status(&test_db, id).await, 0);
        test_db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&manager.character]).await.unwrap();
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        assert!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 6), corp).await.is_err());
        assert_eq!(status(&test_db, id).await, 0);
    }).await;
    cleanup(&db, corp, &[manager, applicant, outsider]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn confirmation_joins_once_and_welcome_mail_is_durable() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    fixture::assign_corporation(&db, manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=10,welcome_mail='Welcome' WHERE corporation_id=$1", &[&corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), manager.character).await;
        client.session.apply(vec![("corpid", Value::Int(corp))]);
        let id = application(&test_db, corp, applicant.character, 0).await;
        rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 6), corp).await.unwrap();
        assert_eq!(status(&test_db, id).await, 6);
        assert_eq!(corp_of(&test_db, applicant.character).await, 1);
        client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
        rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), 1).await.unwrap();
        assert_eq!(corp_of(&test_db, applicant.character).await, corp);
        assert_eq!(status(&test_db, id).await, 2);
        assert!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), corp).await.is_err());
        let row = test_db.query_one("SELECT (SELECT count(*) FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2) AS jobs, (SELECT count(*) FROM game.mail_status s JOIN game.mail_messages m USING(message_id) WHERE s.char_id=$1 AND m.sender_id=$2 AND m.body='Welcome') AS mail", &[&applicant.character, &corp]).await.unwrap();
        assert_eq!(row.get::<_, i64>("jobs"), 1);
        assert_eq!(row.get::<_, i64>("mail"), 1);
        client.session.apply(vec![("charid", Value::Int(manager.character)), ("corpid", Value::Int(corp))]);
        rpc(&mut client, "KickOutMember", &[Value::Int(applicant.character)], corp).await.unwrap();
        assert_ne!(corp_of(&test_db, applicant.character).await, corp);
        let invitation = application(&test_db, corp, applicant.character, 8).await;
        test_db.execute("UPDATE game.corporations SET welcome_mail='' WHERE corporation_id=$1", &[&corp]).await.unwrap();
        let fallback = corp_of(&test_db, applicant.character).await;
        client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(fallback))]);
        rpc(&mut client, "UpdateApplicationOffer", &offer(invitation, applicant.character, corp, 2), fallback).await.unwrap();
        assert_eq!(status(&test_db, invitation).await, 2);
        assert_eq!(corp_of(&test_db, applicant.character).await, corp);
        let row = test_db.query_one("SELECT (SELECT count(*) FROM game.employment_history WHERE char_id=$1 AND corporation_id=$2) AS jobs, (SELECT count(*) FROM game.mail_status s JOIN game.mail_messages m USING(message_id) WHERE s.char_id=$1 AND m.sender_id=$2) AS mail", &[&applicant.character, &corp]).await.unwrap();
        assert_eq!(row.get::<_, i64>("jobs"), 2);
        assert_eq!(row.get::<_, i64>("mail"), 1);
    }).await;
    cleanup(&db, corp, &[manager, applicant]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn forged_application_binding_cannot_change_offer() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let manager = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, manager.character, 1).await.unwrap();
    fixture::assign_corporation(&db, manager.character, corp, ROLE_DIRECTOR).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), manager.character).await;
        let id = application(&test_db, corp, applicant.character, 0).await;
        assert!(rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 6), corp + 10_000_000).await.is_err());
        assert_eq!(status(&test_db, id).await, 0);
        assert_eq!(corp_of(&test_db, applicant.character).await, 1);
    }).await;
    cleanup(&db, corp, &[manager, applicant]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn competing_offers_have_one_winner() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo_a = fixture::create(&db).await.unwrap();
    let ceo_b = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let member_b = fixture::create(&db).await.unwrap();
    let a = fixture::create_corporation(&db, ceo_a.character, 1).await.unwrap();
    let b = fixture::create_corporation(&db, ceo_b.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo_a.character, a, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, ceo_b.character, b, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, member.character, a, 0).await.unwrap();
    fixture::assign_corporation(&db, member_b.character, b, 0).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=10 WHERE corporation_id=ANY($1)", &[&[a, b].as_slice()]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let first = application(&test_db, a, applicant.character, 6).await;
        let second = application(&test_db, b, applicant.character, 6).await;
        let (mut one, _peer1) = tests::client(test_db.clone(), applicant.character).await;
        let (mut two, _peer2) = tests::client(test_db.clone(), applicant.character).await;
        let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel();
        let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
        let (member_tx, mut member_rx) = tokio::sync::mpsc::unbounded_channel();
        let (member_b_tx, mut member_b_rx) = tokio::sync::mpsc::unbounded_channel();
        for world in [&one.world, &two.world] {
            world.register_pilot(applicant.character, app_tx.clone());
            world.register_pilot(ceo_a.character, a_tx.clone());
            world.register_pilot(ceo_b.character, b_tx.clone());
            world.register_pilot(member.character, member_tx.clone());
            world.register_pilot(member_b.character, member_b_tx.clone());
        }
        let args1 = offer(first, applicant.character, a, 2);
        let args2 = offer(second, applicant.character, b, 2);
        let (r1, r2) = tokio::join!(rpc(&mut one, "UpdateApplicationOffer", &args1, 1), rpc(&mut two, "UpdateApplicationOffer", &args2, 1));
        assert_ne!(r1.is_ok(), r2.is_ok());
        let winner = corp_of(&test_db, applicant.character).await;
        assert!(winner == a || winner == b);
        assert_eq!(status(&test_db, first).await, if winner == a { 2 } else { 7 });
        assert_eq!(status(&test_db, second).await, if winner == b { 2 } else { 7 });
        let expected = |corp, id, state, message: &str| ("OnCorporationApplicationChanged".to_owned(), vec![
            Value::Int(corp), Value::Int(applicant.character), Value::Int(id),
            crate::services::keyval([
                ("applicationID", Value::Int(id)), ("corporationID", Value::Int(corp)),
                ("characterID", Value::Int(applicant.character)), ("applicationText", Value::unicode("申请")),
                ("status", Value::Int(state)), ("applicationDateTime", Value::Int(123)),
                ("customMessage", Value::unicode(message)),
            ]),
        ]);
        let event_a = expected(a, first, if winner == a { 2 } else { 7 }, if winner == a { "response" } else { "" });
        let event_b = expected(b, second, if winner == b { 2 } else { 7 }, if winner == b { "response" } else { "" });
        let a_events: Vec<_> = std::iter::from_fn(|| a_rx.try_recv().ok()).filter(|e| e.0 == "OnCorporationApplicationChanged").collect();
        let b_events: Vec<_> = std::iter::from_fn(|| b_rx.try_recv().ok()).filter(|e| e.0 == "OnCorporationApplicationChanged").collect();
        assert_eq!(a_events, vec![event_a.clone()]);
        assert_eq!(b_events, vec![event_b.clone()]);
        assert!(std::iter::from_fn(|| member_rx.try_recv().ok()).all(|e| e.0 != "OnCorporationApplicationChanged"));
        assert!(std::iter::from_fn(|| member_b_rx.try_recv().ok()).all(|e| e.0 != "OnCorporationApplicationChanged"));
        let mut applications = Vec::new();
        while let Ok(event) = app_rx.try_recv() {
            if event.0 == "OnCorporationApplicationChanged" { applications.push(event); }
        }
        assert_eq!(applications.len(), 2);
        assert!(applications.contains(&event_a));
        assert!(applications.contains(&event_b));
    }).await;
    cleanup(&db, a, &[ceo_a, applicant, member]).await;
    cleanup(&db, b, &[ceo_b, member_b]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn last_capacity_slot_has_one_winner() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let one = fixture::create(&db).await.unwrap();
    let two = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=2 WHERE corporation_id=$1", &[&corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let first = application(&test_db, corp, one.character, 6).await;
        let second = application(&test_db, corp, two.character, 8).await;
        let (mut c1, _peer1) = tests::client(test_db.clone(), one.character).await;
        let (mut c2, _peer2) = tests::client(test_db.clone(), two.character).await;
        let args1 = offer(first, one.character, corp, 2);
        let args2 = offer(second, two.character, corp, 2);
        let (r1, r2) = tokio::join!(rpc(&mut c1, "UpdateApplicationOffer", &args1, 1), rpc(&mut c2, "UpdateApplicationOffer", &args2, 1));
        assert_ne!(r1.is_ok(), r2.is_ok());
        assert_ne!(corp_of(&test_db, one.character).await == corp, corp_of(&test_db, two.character).await == corp);
        assert_eq!(status(&test_db, first).await, if r1.is_ok() { 2 } else { 6 });
        assert_eq!(status(&test_db, second).await, if r2.is_ok() { 2 } else { 8 });
    }).await;
    cleanup(&db, corp, &[ceo, one, two]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn welcome_setter_requires_current_manager() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, member.character, corp, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        let (ceo_tx, mut ceo_rx) = tokio::sync::mpsc::unbounded_channel();
        let (member_tx, mut member_rx) = tokio::sync::mpsc::unbounded_channel();
        let (outsider_tx, mut outsider_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(ceo.character, ceo_tx);
        client.world.register_pilot(member.character, member_tx);
        client.world.register_pilot(outsider.character, outsider_tx);
        let before = crate::client::filetime_now();
        rpc(&mut client, "SetCorpWelcomeMail", &[Value::unicode("欢迎")], corp).await.unwrap();
        let after = crate::client::filetime_now();
        let (event, args) = ceo_rx.try_recv().unwrap();
        assert_eq!(event, "OnCorporationWelcomeMailChanged");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], Value::Int(ceo.character));
        let stamp = args[1].as_int().unwrap();
        assert!(stamp >= before && stamp <= after);
        assert_eq!(member_rx.try_recv().unwrap(), (event, args));
        assert!(outsider_rx.try_recv().is_err());
        assert_eq!(rpc(&mut client, "GetCorpWelcomeMail", &[], corp).await.unwrap(), Value::unicode("欢迎"));
        client.session.apply(vec![("charid", Value::Int(outsider.character)), ("corpid", Value::Int(1))]);
        assert!(rpc(&mut client, "SetCorpWelcomeMail", &[Value::unicode("forged")], corp).await.is_err());
        assert_eq!(rpc(&mut client, "GetCorpWelcomeMail", &[], corp).await.unwrap(), Value::unicode("欢迎"));
    }).await;
    cleanup(&db, corp, &[ceo, outsider, member]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn grouped_applications_and_applicant_history_have_complete_rows() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, member.character, corp, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let open = application(&test_db, corp, applicant.character, 0).await;
        let old = application(&test_db, corp, applicant.character, 4).await;
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        let expected = |id, state| crate::services::keyval([
            ("applicationID", Value::Int(id)), ("corporationID", Value::Int(corp)),
            ("characterID", Value::Int(applicant.character)), ("applicationText", Value::unicode("申请")),
            ("status", Value::Int(state)), ("applicationDateTime", Value::Int(123)),
            ("customMessage", Value::unicode("")),
        ]);
        assert_eq!(rpc(&mut client, "GetMyApplications", &[], 1).await.unwrap(), Value::Dict(vec![(Value::Int(corp), Value::list([expected(open, 0)]))]));
        assert_eq!(rpc(&mut client, "GetMyOldApplications", &[], 1).await.unwrap(), Value::list([expected(old, 4)]));
        client.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp))]);
        assert_eq!(rpc(&mut client, "GetApplications", &[], corp).await.unwrap(), Value::Dict(vec![(Value::Int(applicant.character), Value::list([expected(open, 0)]))]));
        assert_eq!(rpc(&mut client, "GetOldApplications", &[], corp).await.unwrap(), Value::list([expected(old, 4)]));
        test_db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
        for method in ["GetApplications", "GetOldApplications"] {
            assert!(rpc(&mut client, method, &[], corp).await.is_err());
            client.session.apply(vec![("charid", Value::Int(member.character)), ("corpid", Value::Int(corp))]);
            assert!(rpc(&mut client, method, &[], corp).await.is_err());
            client.session.apply(vec![("charid", Value::Int(applicant.character)), ("corpid", Value::Int(1))]);
            assert!(rpc(&mut client, method, &[], corp).await.is_err());
            client.session.apply(vec![("charid", Value::Int(ceo.character)), ("corpid", Value::Int(corp))]);
        }
    }).await;
    cleanup(&db, corp, &[ceo, applicant, member]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn legacy_capacity_read_is_transient_and_full_admission_saves_snapshot() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        assert!(crate::dao::corporations::shares::initialize(&test_db, corp, crate::config::game().corporations.initial_shares).await.unwrap());
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        let projected = super::super::corporation_line(&test_db, &client.sde, corp).await.unwrap().unwrap();
        let saved: Option<i64> = test_db.query_one("SELECT member_limit FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get("member_limit");
        assert_eq!(saved, None);
        let limit = super::super::abilities::capacity(&test_db, &client.sde, ceo.character, crate::client::filetime_now()).await.unwrap();
        assert!(projected.contains(&Value::Int(limit)));
        // Fill the player corporation without changing the CEO's skills.
        let mut fillers = Vec::new();
        for _ in 1..limit {
            let person = fixture::create(&test_db).await.unwrap();
            fixture::assign_corporation(&test_db, person.character, corp, 0).await.unwrap();
            fillers.push(person);
        }
        let id = application(&test_db, corp, applicant.character, 6).await;
        let refused = rpc(&mut client, "UpdateApplicationOffer", &offer(id, applicant.character, corp, 2), 1).await;
        let saved: Option<i64> = test_db.query_one("SELECT member_limit FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get("member_limit");
        for person in fillers {
            fixture::assign_corporation(&test_db, person.character, 1, 0).await.unwrap();
            fixture::cleanup(&test_db, person).await.unwrap();
        }
        assert!(refused.is_err());
        assert_eq!(saved, Some(limit));
        assert_eq!(status(&test_db, id).await, 6);
        assert_eq!(corp_of(&test_db, applicant.character).await, 1);
    }).await;
    cleanup(&db, corp, &[ceo, applicant]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn explicit_ability_update_saves_capacity_without_evicting_members() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let member = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, member.character, corp, 0).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        let add = client.sde.dogma.effects_by_name("corporationMemberLimitBonus").next().unwrap().effect_id;
        let multiply = client.sde.dogma.effects_by_name("corporationManagementSkillBoostCorporationMemberBonus").next().unwrap().effect_id;
        let skill = client.sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == add) && ty.effects().iter().any(|e| e.effect_id == multiply)).unwrap().type_id;
        let bonus = client.sde.dogma.attributes_by_name("corporationMemberBonus").next().unwrap();
        let base = client.sde.dogma.attributes_by_name("corporationMemberLimit").next().unwrap().default_value.unwrap();
        let increment = client.sde.types.get(skill).unwrap().attribute(bonus.attribute_id).or(bonus.default_value).unwrap();
        for level in 0_i32..=5 {
            test_db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=$3", &[&ceo.character, &skill, &level]).await.unwrap();
            let expected = (base + increment * f64::from(level)) as i64;
            assert_eq!(rpc(&mut client, "UpdateCorporationAbilities", &[], corp).await.unwrap(), Value::Bool(true));
            let saved: Option<i64> = test_db.query_one("SELECT member_limit FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap().get("member_limit");
            assert_eq!(saved, Some(expected));
        }
        test_db.execute("UPDATE game.skills SET level=0 WHERE character_id=$1 AND type_id=$2", &[&ceo.character, &skill]).await.unwrap();
        assert_eq!(corp_of(&test_db, member.character).await, corp);
        client.session.apply(vec![("charid", Value::Int(outsider.character)), ("corpid", Value::Int(1))]);
        assert!(rpc(&mut client, "UpdateCorporationAbilities", &[], corp).await.is_err());
        assert_eq!(corp_of(&test_db, member.character).await, corp);
    }).await;
    cleanup(&db, corp, &[ceo, member, outsider]).await;
    result.unwrap();
}
