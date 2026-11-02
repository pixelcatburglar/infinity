//! End-to-end welcome delivery, rollback and schema-replay regressions.
use super::*;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

async fn automatic(db: &Db, character: i64, id: i64, list_id: i64, title: &str, rx: &mut UnboundedReceiver<(String, Vec<Value>)>) {
    let statuses = crate::dao::mail::statuses(db, character).await.unwrap();
    let status = statuses.iter().find(|s| s.message_id == id).unwrap();
    assert_eq!((status.status_mask, status.label_mask), (32, 0));
    let headers = crate::dao::mail::headers(db, character, &vec![id]).await.unwrap();
    assert_eq!(headers.len(), 1);
    let header = &headers[0];
    assert_eq!((header.sender_id, header.to_list_id, header.to_corp_or_alliance_id), (list_id, Some(list_id), None));
    assert_eq!(header.to_character_ids, "");
    assert_eq!(header.title, title);
    assert_eq!(rx.try_recv().unwrap(), ("OnMailSent".to_string(), vec![Value::Int(id), Value::Int(list_id), Value::Int(header.sent_date), Value::list([]), Value::Int(list_id), Value::None, Value::unicode(title), Value::Int(32)]));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn muted_welcome_atomicity_and_owned_history_survive_schema_replay() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let third = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let (mut c, _c) = crate::services::tests::client(test_db.clone(), third.character).await;
        let (join_tx, mut join_rx) = unbounded_channel();
        b.world.register_pilot(guest.character, join_tx);
        let (third_tx, mut third_rx) = unbounded_channel();
        c.world.register_pilot(third.character, third_tx);
        let (owner_tx, mut owner_rx) = unbounded_channel();
        let (guest_tx, mut guest_rx) = unbounded_channel();
        a.world.register_pilot(owner.character, owner_tx);
        a.world.register_pilot(guest.character, guest_tx);
        let name = format!("atomic welcome {}", owner.character);
        let id = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(1), Value::Int(0), Value::Int(0)]).await.unwrap().as_int().unwrap();
        let key = Value::Int(id);
        list(&mut a, "SaveWelcomeMail", &[key.clone(), Value::unicode("welcome"), Value::unicode("first body")]).await.unwrap();
        assert!(crate::dao::mail::statuses(&test_db, owner.character).await.unwrap().is_empty());
        assert!(owner_rx.try_recv().is_err());
        let view = list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        assert_eq!(crate::services::value_field(&view, "isMuted"), Some(&Value::Bool(true)));
        let rows = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
        assert_eq!(rows.len(), 1);
        automatic(&test_db, guest.character, rows[0].message_id, id, "welcome", &mut join_rx).await;
        list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        assert_eq!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().len(), 1);
        assert!(join_rx.try_recv().is_err());
        let send = [batch(&[]), key.clone(), Value::None, Value::unicode("ordinary"), Value::unicode("ordinary body"), Value::Int(0), Value::Int(0)];
        assert!(rpc(&mut b, "mailMgr", "SendMail", &send).await.is_err());
        list(&mut a, "SendWelcomeMail", &[key.clone(), Value::unicode("broadcast"), Value::unicode("broadcast body")]).await.unwrap();
        let owner_rows = crate::dao::mail::statuses(&test_db, owner.character).await.unwrap();
        let guest_rows = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
        assert_eq!(owner_rows.len(), 1);
        assert_eq!(guest_rows.len(), 2);
        let broadcast = owner_rows[0].message_id;
        assert!(guest_rows.iter().any(|row| row.message_id == broadcast));
        automatic(&test_db, owner.character, broadcast, id, "broadcast", &mut owner_rx).await;
        automatic(&test_db, guest.character, broadcast, id, "broadcast", &mut guest_rx).await;
        let before = crate::dao::mailing_lists::tests::snapshot(&test_db, id).await.unwrap();
        let count = crate::dao::mailing_lists::member_count(&test_db, id).await.unwrap();
        crate::dao::mailing_lists::tests::reject_list_messages(&test_db, id).await.unwrap();
        let join_result = list(&mut c, "Join", &[Value::unicode(&name)]).await;
        let after_join = crate::dao::mailing_lists::tests::snapshot(&test_db, id).await.unwrap();
        let member = crate::dao::mailing_lists::membership(&test_db, id, third.character).await.unwrap();
        let after_count = crate::dao::mailing_lists::member_count(&test_db, id).await.unwrap();
        let send_result = list(&mut a, "SendWelcomeMail", &[key.clone(), Value::unicode("rejected"), Value::unicode("rejected body")]).await;
        let after_send = crate::dao::mailing_lists::tests::snapshot(&test_db, id).await.unwrap();
        crate::dao::mailing_lists::tests::allow_list_messages(&test_db, id).await.unwrap();
        assert_eq!(member, None);
        assert_eq!(count, after_count);
        assert_eq!(before, after_join);
        assert_eq!(before, after_send);
        assert!(third_rx.try_recv().is_err());
        for result in [join_result, send_result] {
            match result {
                Err(CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
                    Some(crate::db::Error::Postgres(error)) => assert_eq!(error.as_db_error().and_then(|e| e.constraint()), Some(format!("mailing_list_fixture_reject_{id}").as_str())),
                    other => panic!("expected PostgreSQL constraint failure, got {other:?}"),
                },
                other => panic!("expected the fixture constraint failure, got {other:?}"),
            }
        }
        assert!(owner_rx.try_recv().is_err());
        assert!(guest_rx.try_recv().is_err());
        list(&mut c, "Join", &[Value::unicode(&name)]).await.unwrap();
        let third_rows = crate::dao::mail::statuses(&test_db, third.character).await.unwrap();
        assert_eq!(third_rows.len(), 1);
        automatic(&test_db, third.character, third_rows[0].message_id, id, "broadcast", &mut third_rx).await;
        list(&mut a, "SetEntitiesAccess", &[key.clone(), Value::Dict(vec![(Value::Int(guest.character), Value::Int(1))])]).await.unwrap();
        rpc(&mut a, "mailMgr", "SendMail", &send).await.unwrap();
        let history = crate::dao::mailing_lists::tests::snapshot(&test_db, id).await.unwrap();
        assert_eq!(history[0].len(), 1);
        assert_eq!(history[1].len(), 3);
        assert_eq!(history[2].len(), 1);
        assert!(history[3].len() >= 4);
        assert!(!history[4].is_empty());
        for _ in 0..2 {
            let fresh = Db::connect(&crate::config::tests::test_database()).await.unwrap();
            assert_eq!(history, crate::dao::mailing_lists::tests::snapshot(&fresh, id).await.unwrap());
        }
    }).await;
    crate::dao::mailing_lists::tests::allow_owned_list_messages(&db, owner.character).await.unwrap();
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character, third.character]).await.unwrap();
    for f in [owner, guest, third] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
