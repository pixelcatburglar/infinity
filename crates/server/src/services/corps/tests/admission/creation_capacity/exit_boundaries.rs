//! Real player-corporation source admission and founding argument boundaries.
use super::*;
use crate::{client::filetime_now, services::CallError};

/// Snapshot only the participating identities, organizations and their owned effects.
async fn snapshot(db: &Db, ids: &[i64], corps: &[i64], name: &str, job: &str) -> String {
    db.query_one("SELECT jsonb_build_object(
        'characters',(SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.character_id),'[]'::jsonb) FROM game.characters c WHERE c.character_id=ANY($1)),
        'corporations',(SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.corporation_id),'[]'::jsonb) FROM game.corporations c WHERE c.corporation_id=ANY($2) OR c.name=$3),
        'shares',(SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY s.corporation_id,s.shareholder_id),'[]'::jsonb) FROM game.corp_shares s WHERE s.corporation_id=ANY($2) OR s.corporation_id IN (SELECT corporation_id FROM game.corporations WHERE name=$3)),
        'wallets',(SELECT COALESCE(jsonb_agg(to_jsonb(w) ORDER BY w.corporation_id,w.account_key),'[]'::jsonb) FROM game.corp_wallets w WHERE w.corporation_id=ANY($2) OR w.corporation_id IN (SELECT corporation_id FROM game.corporations WHERE name=$3)),
        'employment',(SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.char_id,e.start_date,e.corporation_id),'[]'::jsonb) FROM game.employment_history e WHERE e.char_id=ANY($1)),
        'events',(SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.event_id),'[]'::jsonb) FROM game.corp_member_events e WHERE e.character_id=ANY($1)),
        'roles',(SELECT COALESCE(jsonb_agg(to_jsonb(h) ORDER BY h.history_id),'[]'::jsonb) FROM game.corp_role_history h WHERE h.character_id=ANY($1)),
        'applications',(SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.application_id),'[]'::jsonb) FROM game.corp_applications a WHERE a.character_id=ANY($1) OR a.corporation_id=ANY($2)),
        'notifications',(SELECT COALESCE(jsonb_agg(to_jsonb(n) ORDER BY n.notification_id),'[]'::jsonb) FROM game.notifications n WHERE n.receiver_id=ANY($1)),
        'mail_status',(SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY s.message_id,s.char_id),'[]'::jsonb) FROM game.mail_status s WHERE s.char_id=ANY($1) OR s.message_id IN (SELECT message_id FROM game.mail_messages WHERE sender_id=ANY($2))),
        'mail',(SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.message_id),'[]'::jsonb) FROM game.mail_messages m WHERE m.sender_id=ANY($2) OR m.message_id IN (SELECT message_id FROM game.mail_status WHERE char_id=ANY($1))),
        'pending',(SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.character_id),'[]'::jsonb) FROM game.corp_pending_kicks p WHERE p.character_id=ANY($1)),
        'job',(SELECT COALESCE(jsonb_agg(to_jsonb(j) ORDER BY j.job_id),'[]'::jsonb) FROM game.jobs j WHERE j.job_id=$4)
    )::text AS snapshot", &[&ids, &corps, &name, &job]).await.unwrap().get("snapshot")
}

fn source_refusal(result: CallResult, cooling: bool) {
    match result {
        Err(CallError::User { msg, dict }) => {
            assert_eq!(msg, "CrpCantQuitNotInStasis");
            assert_eq!(dict.iter().any(|(key, _)| key == "deadline"), cooling);
        }
        other => panic!("expected source-exit refusal, got {other:?}"),
    }
}

async fn assert_no_intent(db: &Db, character: i64, job: &str) {
    assert!(db.query_opt("SELECT character_id FROM game.corp_pending_kicks WHERE character_id=$1", &[&character]).await.unwrap().is_none());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().is_none());
}

fn assert_no_notices(receiver: &mut tokio::sync::mpsc::UnboundedReceiver<(String, Vec<Value>)>) {
    assert!(matches!(receiver.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
}

async fn prepare_source(db: &Db, ceo: fixture::Fixture, actor: fixture::Fixture) -> i64 {
    let corp = fixture::create_corporation(db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(db, actor.character, corp, 0).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=10 WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET station_id=1,corp_roles_removed_at=NULL WHERE character_id=$1", &[&actor.character]).await.unwrap();
    corp
}

async fn clear_fixture(db: &Db, actor: fixture::Fixture, others: &[fixture::Fixture], corps: &[i64]) {
    let mut ids = vec![actor.character];
    ids.extend(others.iter().map(|person| person.character));
    let job = crate::services::corps::exit::job_id(actor.character);
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap();
    db.execute("DELETE FROM game.corp_pending_kicks WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_role_history WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.mail_status WHERE char_id=ANY($1) OR message_id IN (SELECT message_id FROM game.mail_messages WHERE sender_id=ANY($2))", &[&ids, &corps]).await.unwrap();
    db.execute("DELETE FROM game.mail_messages WHERE sender_id=ANY($1)", &[&corps]).await.unwrap();
    db.execute("DELETE FROM game.corp_applications WHERE character_id=ANY($1) OR corporation_id=ANY($2)", &[&ids, &corps]).await.unwrap();
    db.execute("DELETE FROM game.employment_history WHERE char_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1,corp_roles=0,corp_roles_hq=0,corp_roles_base=0,corp_roles_other=0 WHERE character_id=ANY($1)", &[&ids]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=ANY($1)", &[&corps]).await.unwrap();
    cleanup_founder(db, actor, &[]).await;
    for person in others { fixture::cleanup(db, *person).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn player_source_founding_checks_exit_and_argument_boundaries() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let source_ceo = fixture::create(&db).await.unwrap();
    let founder = fixture::create(&db).await.unwrap();
    let source = prepare_source(&db, source_ceo, founder).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), founder.character).await;
        client.session.apply(vec![("corpid", Value::Int(source)), ("stationid", Value::Int(1))]);
        let skill = super::super::super::super::SKILL_CORPORATION_MANAGEMENT;
        let level = i32::try_from(crate::config::game().corporations.min_management_skill_level).unwrap().max(1);
        test_db.execute("INSERT INTO game.skills(character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0)", &[&founder.character, &skill, &level]).await.unwrap();
        let starting = super::super::super::super::STARTUP_COST * 2;
        test_db.execute("UPDATE game.characters SET balance=$2::text::numeric WHERE character_id=$1", &[&founder.character, &starting.to_string()]).await.unwrap();
        let name = format!("Source Founder {}", founder.character);
        let ticker = unique_ticker(&test_db, &client.sde, founder.character).await;
        let args = vec![Value::unicode(&name), Value::unicode(ticker), Value::unicode("description"), Value::unicode(""), Value::Float(0.0), Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::None, Value::Bool(false), Value::Bool(false), Value::Float(0.0)];
        let ids = [source_ceo.character, founder.character];
        let corps = [source];
        let job = crate::services::corps::exit::job_id(founder.character);
        let (founder_tx, mut founder_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ceo_tx, mut ceo_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(founder.character, founder_tx);
        client.world.register_pilot(source_ceo.character, ceo_tx);
        assert_no_intent(&test_db, founder.character, &job).await;
        for bad in [args[..14].to_vec(), { let mut values = args.clone(); values.push(Value::None); values }] {
            let before = snapshot(&test_db, &ids, &corps, &name, &job).await;
            assert!(rpc(&mut client, "AddCorporation", &bad, source).await.is_err());
            assert_eq!(snapshot(&test_db, &ids, &corps, &name, &job).await, before);
            assert_no_intent(&test_db, founder.character, &job).await;
            assert_no_notices(&mut founder_rx); assert_no_notices(&mut ceo_rx);
        }
        test_db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&founder.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        let before = snapshot(&test_db, &ids, &corps, &name, &job).await;
        source_refusal(rpc(&mut client, "AddCorporation", &args, source).await, false);
        assert_eq!(snapshot(&test_db, &ids, &corps, &name, &job).await, before);
        assert_no_intent(&test_db, founder.character, &job).await;
        assert_no_notices(&mut founder_rx); assert_no_notices(&mut ceo_rx);
        test_db.execute("UPDATE game.characters SET corp_roles_hq=0,corp_roles_removed_at=$2 WHERE character_id=$1", &[&founder.character, &filetime_now()]).await.unwrap();
        let before = snapshot(&test_db, &ids, &corps, &name, &job).await;
        source_refusal(rpc(&mut client, "AddCorporation", &args, source).await, true);
        assert_eq!(snapshot(&test_db, &ids, &corps, &name, &job).await, before);
        assert_no_intent(&test_db, founder.character, &job).await;
        assert_no_notices(&mut founder_rx); assert_no_notices(&mut ceo_rx);
        test_db.execute("UPDATE game.characters SET corp_roles_removed_at=NULL WHERE character_id=$1", &[&founder.character]).await.unwrap();
        let founded = rpc(&mut client, "AddCorporation", &args, source).await.unwrap().as_int().unwrap();
        assert_ne!(founded, source);
        assert_eq!(corp_of(&test_db, founder.character).await, founded);
        assert_eq!(crate::services::wallets::balance(&test_db, founder.character).await.unwrap(), crate::isk::IskAmount::from_whole_isk(starting).unwrap().checked_sub(crate::isk::IskAmount::from_whole_isk(super::super::super::super::STARTUP_COST).unwrap()).unwrap());
    }).await;
    clear_fixture(&db, founder, &[source_ceo], &[source]).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn player_source_offer_confirmation_checks_exit_before_join() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let source_ceo = fixture::create(&db).await.unwrap();
    let destination_ceo = fixture::create(&db).await.unwrap();
    let applicant = fixture::create(&db).await.unwrap();
    let source = prepare_source(&db, source_ceo, applicant).await;
    let destination = fixture::create_corporation(&db, destination_ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, destination_ceo.character, destination, ROLE_DIRECTOR).await.unwrap();
    db.execute("UPDATE game.corporations SET member_limit=10,welcome_mail='Welcome' WHERE corporation_id=$1", &[&destination]).await.unwrap();
    let offer_id = application(&db, destination, applicant.character, 6).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), applicant.character).await;
        client.session.apply(vec![("corpid", Value::Int(source)), ("stationid", Value::Int(1))]);
        let (applicant_tx, mut applicant_rx) = tokio::sync::mpsc::unbounded_channel();
        let (source_tx, mut source_rx) = tokio::sync::mpsc::unbounded_channel();
        let (destination_tx, mut destination_rx) = tokio::sync::mpsc::unbounded_channel();
        client.world.register_pilot(applicant.character, applicant_tx);
        client.world.register_pilot(source_ceo.character, source_tx);
        client.world.register_pilot(destination_ceo.character, destination_tx);
        let ids = [source_ceo.character, destination_ceo.character, applicant.character];
        let corps = [source, destination];
        let job = crate::services::corps::exit::job_id(applicant.character);
        let args = offer(offer_id, applicant.character, destination, 2);
        assert_no_intent(&test_db, applicant.character, &job).await;
        test_db.execute("UPDATE game.characters SET corp_roles_hq=$2 WHERE character_id=$1", &[&applicant.character, &infinity_core::consts::ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
        let before = snapshot(&test_db, &ids, &corps, "", &job).await;
        source_refusal(rpc(&mut client, "UpdateApplicationOffer", &args, source).await, false);
        assert_eq!(snapshot(&test_db, &ids, &corps, "", &job).await, before);
        assert_eq!(status(&test_db, offer_id).await, 6);
        assert_no_intent(&test_db, applicant.character, &job).await;
        assert_no_notices(&mut applicant_rx); assert_no_notices(&mut source_rx); assert_no_notices(&mut destination_rx);
        test_db.execute("UPDATE game.characters SET corp_roles_hq=0,corp_roles_removed_at=$2 WHERE character_id=$1", &[&applicant.character, &filetime_now()]).await.unwrap();
        let before = snapshot(&test_db, &ids, &corps, "", &job).await;
        source_refusal(rpc(&mut client, "UpdateApplicationOffer", &args, source).await, true);
        assert_eq!(snapshot(&test_db, &ids, &corps, "", &job).await, before);
        assert_eq!(status(&test_db, offer_id).await, 6);
        assert_no_intent(&test_db, applicant.character, &job).await;
        assert_no_notices(&mut applicant_rx); assert_no_notices(&mut source_rx); assert_no_notices(&mut destination_rx);
        test_db.execute("UPDATE game.characters SET corp_roles_removed_at=NULL WHERE character_id=$1", &[&applicant.character]).await.unwrap();
        rpc(&mut client, "UpdateApplicationOffer", &args, source).await.unwrap();
        assert_eq!(corp_of(&test_db, applicant.character).await, destination);
        assert_eq!(status(&test_db, offer_id).await, 2);
    }).await;
    clear_fixture(&db, applicant, &[source_ceo, destination_ceo], &[source, destination]).await;
    result.unwrap();
}
