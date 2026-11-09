//! Snapshot-weighted voting, ballot replay and durable close behavior.
use super::fixtures;
use infinity_core::consts::FILETIME_DAY;
use crate::{dao::corporations::{shares, votes as dao}, db::outbox::Outbox, services::corps::{vote_close, votes}};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn snapshot_claims_survive_transfer_and_ceo_change() {
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
    let options = vec!["赞成".to_string(), "反对".to_string()];
    let mut created = Outbox::<()>::default();
    let id = votes::create_inner(&tx, &mut created, ceo, "议案", "说明", &options, 1, 1).await.unwrap();
    assert!(!created.into_entries().is_empty());
    let job_id = format!("{}:{id}", vote_close::KIND);
    let job = crate::dao::jobs::lock_job(&tx, &job_id).await.unwrap().unwrap();
    assert_eq!(job.due_at, 1 + FILETIME_DAY);
    assert_eq!(job.payload, id.to_string());
    assert!(shares::transfer(&tx, issuer, holder, replacement, 100).await.unwrap());
    let mut cast = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut cast, holder, issuer, id, 0, 2).await.unwrap();
    assert_eq!(dao::ballot(&tx, id, holder).await.unwrap().unwrap().weight, 300);
    assert!(!cast.into_entries().is_empty());
    let mut replay = Outbox::<()>::default();
    votes::cast_inner(&tx, &mut replay, holder, issuer, id, 0, 2).await.unwrap();
    assert!(replay.into_entries().is_empty());
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
    let claimed = crate::dao::jobs::claim(&tx, deadline, deadline + 1, 10000, &[]).await.unwrap();
    let first = claimed.iter().find(|claim| claim.job.id == job_id).unwrap();
    let reclaimed = crate::dao::jobs::claim(&tx, deadline + 2, deadline + 3, 10000, &[]).await.unwrap();
    let second = reclaimed.iter().find(|claim| claim.job.id == job_id).unwrap();
    assert_ne!(first.token, second.token);
    let mut close = Outbox::<()>::default();
    vote_close::close_inner(&tx, &mut close, id, 1 + FILETIME_DAY).await.unwrap();
    assert!(!close.into_entries().is_empty());
    let mut again = Outbox::<()>::default();
    vote_close::close_inner(&tx, &mut again, id, 1 + FILETIME_DAY).await.unwrap();
    assert!(again.into_entries().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn invalid_cases_and_unauthorized_ballots_leave_no_rows() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let outsider = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    let options = vec!["甲".to_string(), "乙".to_string()];
    for (actor, title, options, days) in [
        (outsider, "议案", options.clone(), 1),
        (ceo, "", options.clone(), 1),
        (ceo, "议案", vec!["甲".to_string()], 1),
        (ceo, "议案", options.clone(), 0),
        (ceo, "议案", options.clone(), 31),
    ] {
        let mut effects = Outbox::<()>::default();
        assert!(votes::create_inner(&tx, &mut effects, actor, title, "", &options, days, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
    }
    for (title, description, choices) in [
        ("x".repeat(101), String::new(), options.clone()),
        ("议案".to_string(), "x".repeat(1001), options.clone()),
        ("议案".to_string(), String::new(), vec!["x".repeat(101), "乙".to_string()]),
        ("议案".to_string(), String::new(), vec!["甲".to_string(); 51]),
    ] {
        let mut effects = Outbox::<()>::default();
        assert!(votes::create_inner(&tx, &mut effects, ceo, &title, &description, &choices, 1, 1).await.is_err());
        assert!(effects.into_entries().is_empty());
    }
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_vote_cases WHERE corporation_id=$1", &[&issuer]).await.unwrap().get("count");
    assert_eq!(count, 0);
    let mut effects = Outbox::<()>::default();
    let id = votes::create_inner(&tx, &mut effects, ceo, "议案", "", &options, 1, 1).await.unwrap();
    let mut refused = Outbox::<()>::default();
    assert!(!votes::can_view(&tx, outsider, issuer).await.unwrap());
    assert!(votes::cast_inner(&tx, &mut refused, outsider, issuer, id, 0, 2).await.is_err());
    assert!(votes::cast_inner(&tx, &mut refused, ceo, issuer + 1, id, 0, 2).await.is_err());
    assert!(votes::cast_inner(&tx, &mut refused, ceo, issuer, id, 2, 2).await.is_err());
    assert!(votes::cast_inner(&tx, &mut refused, ceo, issuer, id, 0, 1 + FILETIME_DAY).await.is_err());
    assert!(refused.into_entries().is_empty());
    assert!(dao::ballot(&tx, id, ceo).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn open_case_limit_refuses_without_inserting() {
    let mut client = fixtures::connect().await;
    let tx = client.transaction().await.unwrap();
    let ceo = fixtures::character(&tx).await;
    let issuer = fixtures::corporation(&tx, ceo).await;
    assert!(shares::initialize(&tx, issuer, 1000).await.unwrap());
    tx.execute("INSERT INTO game.corp_vote_cases (corporation_id,creator_id,title,description,starts,ends) SELECT $1,$2,'fixture','',1,$3 FROM generate_series(1,1000)", &[&issuer, &ceo, &(1 + FILETIME_DAY)]).await.unwrap();
    let options = vec!["甲".to_string(), "乙".to_string()];
    let mut effects = Outbox::<()>::default();
    assert!(votes::create_inner(&tx, &mut effects, ceo, "议案", "", &options, 1, 1).await.is_err());
    assert!(effects.into_entries().is_empty());
    let count: i64 = tx.query_one("SELECT count(*) AS count FROM game.corp_vote_cases WHERE corporation_id=$1", &[&issuer]).await.unwrap().get("count");
    assert_eq!(count, 1000);
    tx.rollback().await.unwrap();
}
