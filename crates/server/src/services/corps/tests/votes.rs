//! Snapshot-weighted voting, ballot replay and durable close behavior.
use super::fixtures;
use infinity_core::consts::FILETIME_DAY;
use crate::{dao::corporations::{shares, votes as dao}, db::outbox::Outbox, services::corps::{vote_close, votes}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn snapshot_claims_survive_transfer_and_ceo_change() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::tests::shared_sde(&db).await;
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let holder = fixtures::character(&tx).await;
    let replacement = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    let foreign = fixtures::corporation(&tx, holder).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, holder, 100).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, foreign, 200).await.unwrap());
    assert!(shares::transfer(&tx, issuer, issuer, replacement, 5).await.unwrap());
    let id = fixtures::general_vote_fixture(&tx, issuer, ceo, 1, 1 + FILETIME_DAY).await.unwrap();
    let job_id = format!("{}:{id}", vote_close::KIND);
    let job = crate::dao::jobs::lock_job(&tx, &job_id).await.unwrap().unwrap();
    assert_eq!(job.due_at, 1 + FILETIME_DAY);
    assert_eq!(job.payload, id.to_string());
    assert!(shares::transfer(&tx, issuer, holder, replacement, 100).await.unwrap());
    assert!(crate::dao::corporations::registry::lock_admission(&tx, &[], &[issuer, foreign], &[holder, replacement, ceo]).await.unwrap());
    let mut cast = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut cast, holder, issuer, id, 0, 2).await.unwrap();
    assert_eq!(dao::ballot(&tx, id, holder).await.unwrap().unwrap().weight, 300);
    assert!(!cast.into_entries().is_empty());
    let claims: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_vote_claims WHERE case_id=$1", &[&id]).await.unwrap().get("count");
    let mut replay = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut replay, holder, issuer, id, 0, 2).await.unwrap();
    assert!(replay.into_entries().is_empty());
    let replayed: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_vote_claims WHERE case_id=$1", &[&id]).await.unwrap().get("count");
    assert_eq!(replayed, claims);
    let mut replay = Outbox::<()>::default();
    assert!(votes::cast_inner(&tx, &mut replay, holder, issuer, id, 1, 2).await.is_err());
    tx.execute("UPDATE game.corporations SET ceo_id=$2 WHERE corporation_id=$1", &[&foreign, &replacement]).await.unwrap();
    votes::cast_inner(&tx, &mut replay, replacement, issuer, id, 0, 2).await.unwrap();
    assert_eq!(dao::ballot(&tx, id, replacement).await.unwrap().unwrap().weight, 5);
    votes::cast_inner(&tx, &mut replay, ceo, issuer, id, 1, 2).await.unwrap();
    assert_eq!(dao::ballot(&tx, id, ceo).await.unwrap().unwrap().weight, 0);
    let results = dao::options(&tx, id).await.unwrap();
    assert_eq!((results[0].option_id, results[0].votes_for), (0, 305));
    let deadline = 1 + FILETIME_DAY;
    let others: Vec<String> = tx.query("SELECT job_id FROM game.jobs WHERE job_id<>$1", &[&job_id]).await.unwrap().iter().map(|r| r.get("job_id")).collect();
    let excluded: Vec<&str> = others.iter().map(String::as_str).collect();
    let claimed = crate::dao::jobs::claim(&tx, deadline, deadline + 1, 1, &excluded).await.unwrap();
    let first = claimed.iter().find(|claim| claim.job.id == job_id).unwrap();
    let reclaimed = crate::dao::jobs::claim(&tx, deadline + 2, deadline + 3, 1, &excluded).await.unwrap();
    let second = reclaimed.iter().find(|claim| claim.job.id == job_id).unwrap();
    assert_ne!(first.token, second.token);
    let mut close = Outbox::<()>::default();
    vote_close::close_inner(&tx, &sde, &mut close, id, 1 + FILETIME_DAY).await.unwrap();
    assert!(!close.into_entries().is_empty());
    let mut again = Outbox::<()>::default();
    vote_close::close_inner(&tx, &sde, &mut again, id, 1 + FILETIME_DAY).await.unwrap();
    assert!(again.into_entries().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn invalid_cases_and_unauthorized_ballots_leave_no_rows() {
    use crate::services::tests::{client, fixture};
    use marshal::Value;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let issuer = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, issuer, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
    assert!(shares::initialize(&db, issuer, 1000).await.unwrap());
    let (mut director, _peer) = client(db.clone(), ceo.character).await;
    let (mut stranger, _peer2) = client(db.clone(), outsider.character).await;
    director.session.apply(vec![("corpid", Value::Int(issuer))]);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    director.world.register_pilot(ceo.character, sender);
    let args = |title: String, description: String, choices: Vec<String>, days: i64| [
        Value::unicode(title), Value::unicode(description), Value::Int(4),
        Value::list(choices.into_iter().map(Value::unicode).collect::<Vec<_>>()), Value::Int(days),
    ];
    let choices = vec!["A".to_string(), "B".to_string()];
    assert!(fixtures::vote_call(&mut stranger, "InsertVoteCase", &args("Proposal".into(), "".into(), choices.clone(), 1)).await.is_err());
    for (title, description, options, days) in [
        ("".into(), "".into(), choices.clone(), 1),
        ("Proposal".into(), "".into(), vec!["A".into()], 1),
        ("Proposal".into(), "".into(), choices.clone(), 0),
        ("Proposal".into(), "".into(), choices.clone(), 31),
        ("x".repeat(101), "".into(), choices.clone(), 1),
        ("Proposal".into(), "x".repeat(1001), choices.clone(), 1),
        ("Proposal".into(), "".into(), vec!["x".repeat(101), "B".into()], 1),
        ("Proposal".into(), "".into(), vec!["A".into(); 51], 1),
    ] {
        assert!(fixtures::vote_call(&mut director, "InsertVoteCase", &args(title, description, options, days)).await.is_err());
    }
    let count: i64 = db.query_one("SELECT count(*) AS count FROM game.corp_vote_cases WHERE corporation_id=$1", &[&issuer]).await.unwrap().get("count");
    assert_eq!(count, 0);
    let reply = fixtures::vote_call(&mut director, "InsertVoteCase", &args("Proposal".into(), "".into(), choices, 1)).await.unwrap();
    let Value::Int(id) = marshal::loads(&marshal::dumps(&reply)).unwrap() else { panic!("unexpected case: {reply:?}") };
    assert!(receiver.try_recv().is_ok());
    let state: String = db.query_one("SELECT row_to_json(c)::text || (SELECT COALESCE(string_agg(row_to_json(o)::text,'' ORDER BY o.option_id),'') FROM game.corp_vote_options o WHERE o.case_id=c.id) || (SELECT COALESCE(string_agg(row_to_json(s)::text,'' ORDER BY s.holder_id),'') FROM game.corp_vote_snapshot s WHERE s.case_id=c.id) || (SELECT row_to_json(j)::text FROM game.jobs j WHERE j.job_id=$2) AS state FROM game.corp_vote_cases c WHERE c.id=$1", &[&id, &format!("{}:{id}", vote_close::KIND)]).await.unwrap().get("state");
    assert!(crate::dao::jobs::lock_job(&db, &format!("{}:{id}", vote_close::KIND)).await.unwrap().is_some());
    assert!(fixtures::vote_call(&mut stranger, "InsertVote", &[Value::Int(issuer), Value::Int(id), Value::Int(0)]).await.is_err());
    for (corp, option) in [(issuer + 1, 0), (issuer, 2)] {
        assert!(fixtures::vote_call(&mut director, "InsertVote", &[Value::Int(corp), Value::Int(id), Value::Int(option)]).await.is_err());
    }
    assert!(dao::ballot(&db, id, ceo.character).await.unwrap().is_none());
    let after: String = db.query_one("SELECT row_to_json(c)::text || (SELECT COALESCE(string_agg(row_to_json(o)::text,'' ORDER BY o.option_id),'') FROM game.corp_vote_options o WHERE o.case_id=c.id) || (SELECT COALESCE(string_agg(row_to_json(s)::text,'' ORDER BY s.holder_id),'') FROM game.corp_vote_snapshot s WHERE s.case_id=c.id) || (SELECT row_to_json(j)::text FROM game.jobs j WHERE j.job_id=$2) AS state FROM game.corp_vote_cases c WHERE c.id=$1", &[&id, &format!("{}:{id}", vote_close::KIND)]).await.unwrap().get("state");
    assert_eq!(after, state);
    director.world.unregister_pilot_generation(ceo.character, None);
    fixtures::cleanup_votes(&db, &[issuer]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&issuer]).await.unwrap();
    fixture::cleanup_corporation(&db, issuer, &[ceo.character], 0).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, outsider).await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn open_case_limit_refuses_without_inserting() {
    use crate::services::tests::{client, fixture};
    use marshal::Value;
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let issuer = fixture::create_corporation(&db, ceo.character, 1).await.unwrap();
    fixture::assign_corporation(&db, ceo.character, issuer, infinity_core::consts::ROLE_DIRECTOR).await.unwrap();
    assert!(shares::initialize(&db, issuer, 1000).await.unwrap());
    let now = crate::client::filetime_now();
    db.execute("INSERT INTO game.corp_vote_cases (corporation_id,creator_id,title,description,starts,ends) SELECT $1,$2,'fixture','',$3,$4 FROM generate_series(1,1000)", &[&issuer, &ceo.character, &now, &(now + FILETIME_DAY)]).await.unwrap();
    let (mut director, _peer) = client(db.clone(), ceo.character).await;
    director.session.apply(vec![("corpid", Value::Int(issuer))]);
    let args = [Value::unicode("Proposal"), Value::unicode(""), Value::Int(4), Value::list(vec![Value::unicode("A"), Value::unicode("B")]), Value::Int(1)];
    assert!(fixtures::vote_call(&mut director, "InsertVoteCase", &args).await.is_err());
    let count: i64 = db.query_one("SELECT count(*) AS count FROM game.corp_vote_cases WHERE corporation_id=$1", &[&issuer]).await.unwrap().get("count");
    assert_eq!(count, 1000);
    fixtures::cleanup_votes(&db, &[issuer]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&issuer]).await.unwrap();
    fixture::cleanup_corporation(&db, issuer, &[ceo.character], 0).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
}
