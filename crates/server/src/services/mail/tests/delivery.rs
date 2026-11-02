//! List delivery, persisted headers and character-scoped status selectors.
use super::*;
use std::io::Read;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn list_delivery_and_by_list_status_survive_reconnect() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let name = format!("delivery {}", owner.character);
        let other_name = format!("other {}", owner.character);
        let create = |n: &str| vec![Value::unicode(n), Value::Int(1), Value::Int(1), Value::Int(0)];
        let id = list(&mut a, "Create", &create(&name)).await.unwrap().as_int().unwrap();
        let other = list(&mut a, "Create", &create(&other_name)).await.unwrap().as_int().unwrap();
        list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        list(&mut b, "Join", &[Value::unicode(&other_name)]).await.unwrap();
        let send = |list_id, chars| vec![chars, Value::Int(list_id), Value::None, Value::unicode("subject"), Value::unicode("persistent body"), Value::Int(0), Value::Int(0)];
        let msg = rpc(&mut a, "mailMgr", "SendMail", &send(id, batch(&[guest.character, guest.character]))).await.unwrap().as_int().unwrap();
        let other_msg = rpc(&mut a, "mailMgr", "SendMail", &send(other, batch(&[]))).await.unwrap().as_int().unwrap();
        let rows = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
        let row = rows.iter().find(|r| r.message_id == msg).unwrap();
        assert_eq!((row.status_mask, row.label_mask), (0, 1));
        let own = crate::dao::mail::statuses(&test_db, owner.character).await.unwrap();
        let row = own.iter().find(|r| r.message_id == msg).unwrap();
        assert_eq!((row.status_mask, row.label_mask), (0, 2));
        assert!(crate::dao::mail::statuses(&test_db, outsider.character).await.unwrap().is_empty());
        let headers = crate::dao::mail::headers(&test_db, guest.character, &vec![msg]).await.unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].to_list_id, Some(id));
        assert_eq!(headers[0].to_character_ids, guest.character.to_string());
        assert!(rpc(&mut a, "mailMgr", "SendMail", &send(id, batch(&vec![guest.character; MAX_RECIPIENTS + 1]))).await.is_err());
        let mut both = send(id, batch(&[])); both[2] = Value::Int(1);
        assert!(rpc(&mut a, "mailMgr", "SendMail", &both).await.is_err());
        for method in ["MarkAsReadByList", "MoveToTrashByList", "MarkAsUnreadByList"] {
            assert_eq!(rpc(&mut b, "mailMgr", method, &[Value::Int(id)]).await.unwrap(), Value::None);
        }
        let statuses = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
        assert_eq!(statuses.iter().find(|r| r.message_id == msg).unwrap().status_mask, 9);
        assert_eq!(statuses.iter().find(|r| r.message_id == other_msg).unwrap().status_mask, 0);
        assert_eq!(crate::dao::mail::statuses(&test_db, owner.character).await.unwrap().iter().find(|r| r.message_id == msg).unwrap().status_mask, 0);
        list(&mut a, "SaveWelcomeMail", &[Value::Int(id), Value::unicode("welcome"), Value::unicode("body")]).await.unwrap();
        list(&mut a, "SetEntitiesAccess", &[Value::Int(id), Value::Dict(vec![(Value::Int(guest.character), Value::Int(1))])]).await.unwrap();
        let before = crate::dao::mailing_lists::get_info(&test_db, id).await.unwrap().unwrap();
        let before_members = crate::dao::mailing_lists::members(&test_db, id).await.unwrap().into_iter().map(|m| (m.character_id, m.member_access)).collect::<Vec<_>>();
        let before_access = crate::dao::mailing_lists::access_entries(&test_db, id).await.unwrap().into_iter().map(|a| (a.entity_id, a.entity_kind, a.access)).collect::<Vec<_>>();
        let fresh = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let after = crate::dao::mailing_lists::get_info(&fresh, id).await.unwrap().unwrap();
        assert_eq!((&before.name_key, &before.display_name, before.owner_id, before.default_access, before.default_member_access, &before.welcome_title, &before.welcome_body, before.deleted_at), (&after.name_key, &after.display_name, after.owner_id, after.default_access, after.default_member_access, &after.welcome_title, &after.welcome_body, after.deleted_at));
        assert_eq!(before_members, crate::dao::mailing_lists::members(&fresh, id).await.unwrap().into_iter().map(|m| (m.character_id, m.member_access)).collect::<Vec<_>>());
        assert_eq!(before_access, crate::dao::mailing_lists::access_entries(&fresh, id).await.unwrap().into_iter().map(|a| (a.entity_id, a.entity_kind, a.access)).collect::<Vec<_>>());
        let persisted = crate::dao::mail::statuses(&fresh, guest.character).await.unwrap();
        let previous = statuses.iter().find(|r| r.message_id == msg).unwrap();
        let current = persisted.iter().find(|r| r.message_id == msg).unwrap();
        assert_eq!((previous.status_mask, previous.label_mask), (current.status_mask, current.label_mask));
        let (mut reconnected, _peer) = crate::services::tests::client(fresh, guest.character).await;
        let header = rpc(&mut reconnected, "mailMgr", "GetMailHeaders", &[batch(&[msg])]).await.unwrap(); wire(&header);
        let body = rpc(&mut reconnected, "mailMgr", "GetBody", &[Value::Int(msg), Value::Bool(false)]).await.unwrap();
        let bytes = match body { Value::Bytes(bytes) => bytes, _ => panic!("expected compressed body") };
        let mut text = String::new();
        flate2::read::ZlibDecoder::new(bytes.as_slice()).read_to_string(&mut text).unwrap();
        assert_eq!(text, "persistent body");
        list(&mut a, "Delete", &[Value::Int(id)]).await.unwrap();
        assert!(rpc(&mut reconnected, "mailMgr", "GetBody", &[Value::Int(msg), Value::Bool(false)]).await.unwrap() != Value::None);
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character, outsider.character]).await.unwrap();
    for f in [owner, guest, outsider] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
