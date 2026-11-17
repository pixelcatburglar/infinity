//! A newer publication must survive a saved earlier run identity.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database"]
async fn saved_earlier_identity_never_overrides_a_newer_schedule() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let mut claimed = job(1, 1);
    claimed.id = format!("{JOB_ID}:test:{}", filetime_now());
    let result = crate::tests::catch_scenario(async {
        jobs::schedule(&db, &claimed, 1).await.unwrap();
        let claimed = jobs::lock_job(&db, &claimed.id).await.unwrap().unwrap();
        let mut published = claimed.clone();
        published.due_at = 300;
        published.payload = Payload { policy_ordinal: 2 }.text();
        jobs::schedule(&db, &published, 100).await.unwrap();
        let mut next = claimed.clone();
        next.due_at = 400;
        reschedule(&db, claimed.clone(), next, 100).await.unwrap();
        let stored = jobs::lock_job(&db, &claimed.id).await.unwrap().unwrap();
        assert_eq!((stored.id, stored.kind, stored.due_at, stored.payload),
            (published.id, published.kind, published.due_at, published.payload));
    }).await;
    jobs::cancel(&db, &claimed.id).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
