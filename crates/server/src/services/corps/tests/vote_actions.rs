//! End-to-end sanctioned share issuance and its execution deadline.
use super::fixtures::{cleanup_votes, typed_vote_options, vote_call};
use crate::{dao::corporations::{sanctions, shares, votes}, db::Db, scheduler::Job,
    services::{corps::{vote_close, vote_request}, tests::{self, fixture}}};
use infinity_core::consts::{FILETIME_DAY, ROLE_DIRECTOR};
use marshal::Value;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn issuance_is_authorized_once_and_preserves_the_snapshot() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let director = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, corp, ROLE_DIRECTOR).await.unwrap();
    fixture::assign_corporation(&db, director.character, corp, ROLE_DIRECTOR).await.unwrap();
    assert!(shares::initialize(&db, corp, 1000).await.unwrap());
    assert!(shares::transfer(&db, corp, corp, ceo.character, 100).await.unwrap());
    let (mut client, _peer) = tests::client(db.clone(), ceo.character).await;
    client.session.apply(vec![("corpid", Value::Int(corp))]);
    let (mut other, _other_peer) = tests::client_with_sde(db.clone(), director.character, client.sde.clone()).await;
    other.session.apply(vec![("corpid", Value::Int(corp))]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.world.register_pilot(ceo.character, tx);
    let options = typed_vote_options([
        "UI/Corporations/CorporationWindow/Politics/CreateShares",
        "UI/Corporations/CorporationWindow/Politics/DoNotCreateShares",
    ], [Some(200), None, None]);
    let args = [Value::unicode("Issue"), Value::unicode(""), Value::Int(vote_request::VOTE_SHARES), options, Value::Int(1)];
    let Value::Int(case) = vote_call(&mut client, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
    vote_call(&mut client, "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(0)]).await.unwrap();
    let end = crate::client::filetime_now() - 1;
    db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&case, &end]).await.unwrap();
    let job = Job { id: format!("{}:{case}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: end, payload: case.to_string() };
    vote_close::handle(db.clone(), client.world.clone(), job).await.unwrap();
    let before = votes::ballot(&db, case, ceo.character).await.unwrap().unwrap().weight;
    assert_eq!(before, 100);
    let action = sanctions::get(&db, case).await.unwrap().unwrap();
    assert_eq!(action.expires, end + crate::config::game().corporations.vote_action_window_secs * infinity_core::consts::FILETIME_SECOND);
    assert!(!sanctions::mark_acted(&db, case, action.expires).await.unwrap());
    assert!(vote_call(&mut other, "ActivateSanctionedAction", &[Value::Int(case)]).await.is_err());
    db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    assert!(vote_call(&mut client, "ActivateSanctionedAction", &[Value::Int(case)]).await.is_err());
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo.character, &ROLE_DIRECTOR]).await.unwrap();
    db.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&corp, &director.character]).await.unwrap();
    assert!(vote_call(&mut client, "ActivateSanctionedAction", &[Value::Int(case)]).await.is_err());
    db.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&corp, &ceo.character]).await.unwrap();
    while rx.try_recv().is_ok() {}
    vote_call(&mut client, "ActivateSanctionedAction", &[Value::Int(case)]).await.unwrap();
    assert_eq!(shares::issued_total(&db, corp).await.unwrap(), Some(1200));
    assert_eq!(shares::balance(&db, corp, corp).await.unwrap(), 1100);
    assert_eq!(shares::shareholders(&db, corp).await.unwrap().iter().map(|h| h.shares).sum::<i64>(), 1200);
    assert_eq!(votes::ballot(&db, case, ceo.character).await.unwrap().unwrap().weight, before);
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(events.iter().filter(|(name, _)| name == "OnShareChange").count(), 1);
    vote_call(&mut client, "ActivateSanctionedAction", &[Value::Int(case)]).await.unwrap();
    assert!(!std::iter::from_fn(|| rx.try_recv().ok()).any(|(name, _)| name == "OnShareChange" || name == "OnCorporationChanged"));
    assert_eq!(shares::issued_total(&db, corp).await.unwrap(), Some(1200));
    assert!(sanctions::get(&db, case).await.unwrap().unwrap().acted_upon);

    // A second decision that closes after its original window is not given a fresh window.
    let Value::Int(expired) = vote_call(&mut client, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
    vote_call(&mut client, "InsertVote", &[Value::Int(corp), Value::Int(expired), Value::Int(0)]).await.unwrap();
    let old_end = crate::client::filetime_now() - 30 * FILETIME_DAY;
    db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&expired, &old_end]).await.unwrap();
    let job = Job { id: format!("{}:{expired}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: old_end, payload: expired.to_string() };
    vote_close::handle(db.clone(), client.world.clone(), job).await.unwrap();
    let action = sanctions::get(&db, expired).await.unwrap().unwrap();
    assert!(action.expires < crate::client::filetime_now());
    assert!(action.expired_at.is_some());
    assert!(vote_call(&mut client, "ActivateSanctionedAction", &[Value::Int(expired)]).await.is_err());
    assert_eq!(shares::issued_total(&db, corp).await.unwrap(), Some(1200));

    assert_eq!(action.expires, old_end + crate::config::game().corporations.vote_action_window_secs * infinity_core::consts::FILETIME_SECOND);
    cleanup_votes(&db, &[corp]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1)", &[&[ceo.character, director.character].as_slice()]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[ceo.character, director.character], 0).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, director).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE and external flock"]
async fn kick_decisions_distinguish_departure_from_pending_roles() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let ready = fixture::create(&db).await.unwrap();
    let held = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, ceo.character, 60000001).await.unwrap();
    for (person, roles) in [(ceo, ROLE_DIRECTOR), (ready, 0), (held, ROLE_DIRECTOR)] {
        fixture::assign_corporation(&db, person.character, corp, roles).await.unwrap();
    }
    let ids = [ceo.character, ready.character, held.character];
    let marker = super::fixtures::notification_marker(&db, &ids).await;
    assert!(shares::initialize(&db, corp, 1000).await.unwrap());
    assert!(shares::transfer(&db, corp, corp, ceo.character, 100).await.unwrap());
    let (mut client, _peer) = tests::client(db.clone(), ceo.character).await;
    client.session.apply(vec![("corpid", Value::Int(corp)), ("stationid", Value::Int(60000001))]);
    let outsider = fixture::create(&db).await.unwrap();
    for target in [ceo.character, outsider.character] {
        let options = typed_vote_options([
            "UI/Corporations/CorporationWindow/Politics/ExpelFromCorporation",
            "UI/Corporations/CorporationWindow/Politics/DontExpelFromCorporation",
        ], [Some(target), None, None]);
        let args = [Value::unicode("Expel"), Value::unicode(""), Value::Int(vote_request::VOTE_KICK), options, Value::Int(1)];
        assert!(vote_call(&mut client, "InsertVoteCase", &args).await.is_err());
    }
    for (target, pending) in [(ready.character, false), (held.character, true)] {
        let options = typed_vote_options([
            "UI/Corporations/CorporationWindow/Politics/ExpelFromCorporation",
            "UI/Corporations/CorporationWindow/Politics/DontExpelFromCorporation",
        ], [Some(target), None, None]);
        let args = [Value::unicode("Expel"), Value::unicode(""), Value::Int(vote_request::VOTE_KICK), options, Value::Int(1)];
        let Value::Int(case) = vote_call(&mut client, "InsertVoteCase", &args).await.unwrap() else { panic!("case ID") };
        vote_call(&mut client, "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(0)]).await.unwrap();
        let end = crate::client::filetime_now() - 1;
        db.execute("UPDATE game.corp_vote_cases SET ends=$2 WHERE id=$1", &[&case, &end]).await.unwrap();
        let job = Job { id: format!("{}:{case}", vote_close::KIND), kind: vote_close::KIND.into(), due_at: end, payload: case.to_string() };
        vote_close::handle(db.clone(), client.world.clone(), job).await.unwrap();
        vote_call(&mut client, "ActivateSanctionedAction", &[Value::Int(case)]).await.unwrap();
        assert!(sanctions::get(&db, case).await.unwrap().unwrap().acted_upon);
        let row = db.query_one("SELECT corporation_id,corp_roles FROM game.characters WHERE character_id=$1", &[&target]).await.unwrap();
        assert_eq!(row.get::<_, i64>("corporation_id") == corp, pending);
        if pending { assert_ne!(row.get::<_, i64>("corp_roles") & ROLE_DIRECTOR, 0); }
        assert_eq!(crate::dao::corporations::pending_kicks::get(&db, corp, target).await.unwrap().is_some(), pending);
        let notices = db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&target, &marker]).await.unwrap();
        assert_eq!(notices.len(), usize::from(!pending));
    }
    db.execute("UPDATE game.characters SET corp_roles=0 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    super::super::pending_kicks::trigger(&db, &client.world, held.character).await.unwrap();
    assert!(crate::dao::corporations::pending_kicks::get(&db, corp, held.character).await.unwrap().is_none());
    assert_eq!(db.query_one("SELECT corporation_id FROM game.characters WHERE character_id=$1", &[&held.character]).await.unwrap().get::<_, i64>("corporation_id"), corp);
    assert!(db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1 AND type_id=92 AND notification_id>$2", &[&held.character, &marker]).await.unwrap().is_empty());
    cleanup_votes(&db, &[corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_pending_kicks WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("corporations.pending_kick:{}", held.character)]).await.unwrap();
    db.execute("DELETE FROM game.corp_member_events WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_role_history WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.notifications WHERE receiver_id=ANY($1) AND notification_id>$2", &[&ids.as_slice(), &marker]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &ids, 0).await.unwrap();
    for person in [ceo, ready, held] { fixture::cleanup(&db, person).await.unwrap(); }
    fixture::cleanup(&db, outsider).await.unwrap();
}
