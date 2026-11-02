//! Concurrent mutations share the list lock and unique active name.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn concurrent_create_and_join_delete_are_atomic() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut g, _g) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let name = format!("race {}", owner.character);
        let args = vec![Value::unicode(&name), Value::Int(1), Value::Int(1), Value::Int(0)];
        let (x, y) = tokio::join!(list(&mut a, "Create", &args), list(&mut b, "Create", &args));
        assert_ne!(x.is_ok(), y.is_ok());
        let id = x.or(y).unwrap().as_int().unwrap();
        let join_args = [Value::unicode(&name)];
        let delete_args = [Value::Int(id)];
        let (joined, deleted) = tokio::join!(list(&mut g, "Join", &join_args), list(&mut a, "Delete", &delete_args));
        assert!(deleted.is_ok());
        if joined.is_err() { assert!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().is_empty()); }
        assert!(crate::dao::mailing_lists::members(&test_db, id).await.unwrap().is_empty());
        assert!(crate::dao::mailing_lists::get_info(&test_db, id).await.unwrap().unwrap().deleted_at.is_some());
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn send_snapshot_serializes_with_leave_and_kick() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut sender, _sender) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut manager, _manager) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut member, _member) = crate::services::tests::client(test_db.clone(), guest.character).await;
        for kick in [false, true] {
            let name = format!("snapshot {} {kick}", owner.character);
            let id = list(&mut manager, "Create", &[Value::unicode(&name), Value::Int(1), Value::Int(1), Value::Int(0)]).await.unwrap().as_int().unwrap();
            list(&mut member, "Join", &[Value::unicode(&name)]).await.unwrap();
            let send_args = [batch(&[]), Value::Int(id), Value::None, Value::unicode("snapshot"), Value::unicode("body"), Value::Int(0), Value::Int(0)];
            let leave_args = [Value::Int(id)];
            let kick_args = [Value::Int(id), batch(&[guest.character])];
            let remove = async {
                if kick { list(&mut manager, "KickMembers", &kick_args).await }
                else { list(&mut member, "Leave", &leave_args).await }
            };
            let (sent, removed) = tokio::join!(rpc(&mut sender, "mailMgr", "SendMail", &send_args), remove);
            let message = sent.unwrap().as_int().unwrap();
            removed.unwrap();
            assert_eq!(crate::dao::mailing_lists::membership(&test_db, id, guest.character).await.unwrap(), None);
            let statuses = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
            let matching: Vec<_> = statuses.iter().filter(|r| r.message_id == message).collect();
            assert!(matching.len() <= 1);
            if let Some(status) = matching.first() {
                assert_eq!((status.status_mask, status.label_mask), (0, 0));
                let headers = crate::dao::mail::headers(&test_db, guest.character, &vec![message]).await.unwrap();
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].to_list_id, Some(id));
                assert!(headers[0].to_character_ids.is_empty());
            }
            let after = rpc(&mut sender, "mailMgr", "SendMail", &send_args).await.unwrap().as_int().unwrap();
            let statuses = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
            assert_eq!(statuses.iter().filter(|r| r.message_id == message).count(), matching.len());
            assert!(!statuses.iter().any(|r| r.message_id == after));
        }
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
