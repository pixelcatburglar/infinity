//! Transactional delivery and notification regressions.
use super::*;
use crate::db::outbox::{Effect, Notification};
use crate::services::tests::fixture;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[test]
fn content_limits_are_not_truncated() {
    crate::tests::init_config();
    let cap = usize::try_from(crate::config::game().mail.title_max_len).unwrap().min(MAX_TITLE);
    assert!(validate_content(&"x".repeat(cap), &"x".repeat(MAX_BODY)).is_ok());
    assert!(validate_content(&"x".repeat(cap + 1), "").is_err());
    assert!(validate_content("", &"x".repeat(MAX_BODY + 1)).is_err());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn delivery_outbox_deduplicates_and_rollback_never_dispatches() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sender = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let input = Arc::new(Delivery { sender: sender.character, to_chars: vec![recipient.character], to_list: None, to_corp: None, title: "subject".into(), body: "body".into(), sent: 1, recipients: vec![recipient.character, recipient.character], automated: false });
        let dispatched = Arc::new(AtomicBool::new(false));
        let flag = dispatched.clone();
        let rollback: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| { let input = Arc::clone(&input); Box::pin(async move { deliver(tx, outbox, &input).await?; Err(crate::services::notify_error("rollback")) }) },
            |(), _| async move { flag.store(true, Ordering::SeqCst); Ok(()) },
        ).await;
        assert!(rollback.is_err());
        assert!(!dispatched.load(Ordering::SeqCst));
        assert!(mail::statuses(&test_db, sender.character).await.unwrap().is_empty());
        assert!(mail::statuses(&test_db, recipient.character).await.unwrap().is_empty());
        let (id, entries) = test_db.transaction(
            |tx, outbox: &mut Outbox| { let input = Arc::clone(&input); Box::pin(async move { deliver(tx, outbox, &input).await }) },
            |id, outbox| async move { Ok((id, outbox.into_entries())) },
        ).await.unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            Effect::Notification(Notification::Pilot { char_id, event, args }) => {
                assert_eq!(*char_id, recipient.character);
                assert_eq!(event, "OnMailSent");
                assert_eq!(args, &vec![Value::Int(id), Value::Int(sender.character), Value::Int(1), Value::list([Value::Int(recipient.character)]), Value::None, Value::None, Value::unicode("subject"), Value::Int(0)]);
            }
            _ => panic!("expected one pilot notification"),
        }
        assert_eq!(mail::statuses(&test_db, recipient.character).await.unwrap().len(), 1);
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, sender.character, &[sender.character, recipient.character]).await.unwrap();
    for f in [sender, recipient] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn automated_list_delivery_has_exact_notifications_and_no_sender_copy() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let recipient = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let list = crate::dao::mailing_lists::create(&test_db, &format!("automated{}", owner.character), "automated", owner.character, 1, 1, 0).await.unwrap();
        let input = Arc::new(Delivery { sender: list.list_id, to_chars: vec![recipient.character], to_list: Some(list.list_id), to_corp: None, title: "welcome".into(), body: "body".into(), sent: 1, recipients: vec![owner.character, recipient.character, recipient.character], automated: true });
        let (id, entries) = test_db.transaction(
            |tx, outbox: &mut Outbox| { let input = Arc::clone(&input); Box::pin(async move { deliver(tx, outbox, &input).await }) },
            |id, outbox| async move { Ok((id, outbox.into_entries())) },
        ).await.unwrap();
        assert_eq!(entries.len(), 2);
        for character in [owner.character, recipient.character] {
            let effects: Vec<_> = entries.iter().filter(|effect| matches!(effect, Effect::Notification(Notification::Pilot { char_id, .. }) if *char_id == character)).collect();
            assert_eq!(effects.len(), 1);
            match effects[0] {
                Effect::Notification(Notification::Pilot { event, args, .. }) => {
                    assert_eq!(event, "OnMailSent");
                    assert_eq!(args, &vec![Value::Int(id), Value::Int(list.list_id), Value::Int(1), Value::list([Value::Int(recipient.character)]), Value::Int(list.list_id), Value::None, Value::unicode("welcome"), Value::Int(32)]);
                }
                _ => panic!("expected pilot notification"),
            }
            let rows = mail::statuses(&test_db, character).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].status_mask, 32);
            assert_eq!(rows[0].label_mask, if character == recipient.character { 1 } else { 0 });
        }
        assert!(mail::statuses(&test_db, list.list_id).await.unwrap().is_empty());
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, recipient.character]).await.unwrap();
    for f in [owner, recipient] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
