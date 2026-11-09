//! Slash grammar and durable job operator regressions; database tests require a
//! dedicated database and must run serially without a scheduler process.
use super::*;
use crate::scheduler::Job;

#[test]
fn grammar_requires_exact_arguments() {
    assert_eq!(parse(&[]), Some(Command::List));
    assert_eq!(parse(&["retry".into(), "a".into()]), Some(Command::Retry("a")));
    assert_eq!(parse(&["cancel".into(), "a".into()]), Some(Command::Cancel("a")));
    for args in [&["list"][..], &["list", "extra"][..], &["retry"][..], &["retry", ""][..], &["cancel"][..], &["cancel", "a", "extra"][..], &["unknown"][..]] {
        assert_eq!(parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()), None);
    }
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL; run serially without a scheduler"]
async fn cancelled_periodic_job_is_restored_by_seed() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    jobs::tests::require_job_free_database(&db).await;
    for kind in scheduler::PERIODIC_KINDS {
        assert!(jobs::lock_job(&db, kind).await.unwrap().is_none(),
            "periodic job {kind} already exists; use a dedicated database without fixed periodic IDs");
    }
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        scheduler::seed(&test_db).await.unwrap();
        assert!(command(&test_db, &["cancel".into(), scheduler::BILLS.into()]).await.is_ok());
        assert!(jobs::lock_job(&test_db, scheduler::BILLS).await.unwrap().is_none());
        scheduler::seed(&test_db).await.unwrap();
        for kind in scheduler::PERIODIC_KINDS {
            let job = jobs::lock_job(&test_db, kind).await.unwrap().unwrap();
            assert_eq!(job.id, kind);
            assert_eq!(job.kind, kind);
            assert_eq!(jobs::tests::inspect(&test_db, kind).await.unwrap().state, "scheduled");
        }
    }).await;
    for kind in scheduler::PERIODIC_KINDS {
        scheduler::cancel(&db, kind).await.unwrap();
    }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL; run serially without a scheduler"]
async fn operator_stats_retry_and_cancel() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let existing = jobs::tests::existing_ids(&db).await.unwrap();
    let now = filetime_now();
    let id = format!("jobs-command-test:{}:{now}", std::process::id());
    assert!(!existing.contains(&id), "test job ID already exists: {id}");
    let test_db = db.clone();
    let test_id = id.clone();
    let result = tokio::spawn(async move {
        let excluded: Vec<&str> = existing.iter().map(String::as_str).collect();
        let job = Job { id: test_id.clone(), kind: test_id.clone(), due_at: now, payload: "{\"version\":1}".into() };
        assert_eq!(scheduler::schedule(&test_db, &job).await.unwrap(), 1);
        let counts = jobs::counts(&test_db).await.unwrap();
        assert!(counts.iter().any(|c| c.kind == job.kind && c.state == "scheduled" && c.count == 1));
        let stored = jobs::lock_job(&test_db, &test_id).await.unwrap().unwrap();
        assert_eq!(stored.id, test_id);
        assert_eq!(stored.kind, job.kind);
        assert_eq!(jobs::tests::inspect(&test_db, &test_id).await.unwrap().state, "scheduled");
        let claim = jobs::claim(&test_db, now, now + 1, 1, &excluded).await.unwrap().pop().unwrap();
        assert_eq!(claim.job.id, test_id);
        assert!(retry(&test_db, &test_id).await.unwrap());
        assert_eq!(jobs::ack(&test_db, &test_id, claim.token).await.unwrap(), 0);
        let row = jobs::tests::inspect(&test_db, &test_id).await.unwrap();
        assert_eq!(row.attempt, 1);
        let claim = jobs::claim(&test_db, filetime_now(), i64::MAX, 1, &excluded).await.unwrap().pop().unwrap();
        assert_eq!(claim.job.id, test_id);
        assert_eq!(claim.job.payload, job.payload);
        assert_eq!(jobs::fail(&test_db, &test_id, claim.token, now, now, "failure", true).await.unwrap(), 1);
        assert!(retry(&test_db, &test_id).await.unwrap());
        let row = jobs::tests::inspect(&test_db, &test_id).await.unwrap();
        assert_eq!(row.attempt, 0);
        assert_eq!(row.last_error, None);
        let running = jobs::claim(&test_db, filetime_now(), i64::MAX, 1, &excluded).await.unwrap().pop().unwrap();
        assert_eq!(running.job.id, test_id);
        assert!(command(&test_db, &["cancel".into(), test_id.clone()]).await.is_ok());
        assert_eq!(jobs::ack(&test_db, &test_id, running.token).await.unwrap(), 0);
        assert!(!retry(&test_db, &test_id).await.unwrap());
        assert!(command(&test_db, &["cancel".into(), test_id.clone()]).await.is_err());
    }).await;
    scheduler::cancel(&db, &id).await.unwrap();
    result.unwrap();
}
