//! Concurrent admission at the membership limit.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn concurrent_join_for_last_slot_has_one_winner() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let first = fixture::create(&db).await.unwrap();
    let second = fixture::create(&db).await.unwrap();
    let dummy = match crate::dao::mailing_lists::tests::dummy_characters(&db, owner.account, (MAX_MEMBERS - 2) as i64).await {
        Ok(dummy) => dummy,
        Err(error) => {
            let mut cleanup_errors = Vec::new();
            for f in [owner, first, second] {
                if let Err(error) = fixture::cleanup(&db, f).await { cleanup_errors.push(error); }
            }
            panic!("could not create capacity characters: {error}; cleanup errors: {cleanup_errors:?}");
        }
    };
    let seeded = dummy.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), first.character).await;
        let (mut c, _c) = crate::services::tests::client(test_db.clone(), second.character).await;
        let name = format!("capacity {}", owner.character);
        let id = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(1), Value::Int(1), Value::Int(0)]).await.unwrap().as_int().unwrap();
        crate::dao::mailing_lists::tests::seed_members(&test_db, id, &seeded).await.unwrap();
        let args = [Value::unicode(&name)];
        let (x, y) = tokio::join!(list(&mut b, "Join", &args), list(&mut c, "Join", &args));
        assert_ne!(x.is_ok(), y.is_ok());
        assert!(matches!(if x.is_err() { x } else { y }, Err(CallError::User { .. })));
        assert_eq!(crate::dao::mailing_lists::member_count(&test_db, id).await.unwrap(), MAX_MEMBERS as i64);
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, first.character, second.character]).await.unwrap();
    crate::dao::mailing_lists::tests::delete_dummy_characters(&db, &dummy).await.unwrap();
    for f in [owner, first, second] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
