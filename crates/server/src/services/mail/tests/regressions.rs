//! Regression coverage for the shared scripted-mail entry point.
use super::*;
use crate::services::mail::delivery::{send_mail, SendMail};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn shared_send_mail_uses_database_corporation_and_preserves_personal_status() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let (corp, other, alliance) = match crate::dao::mailing_lists::tests::organizations(&db, owner.character).await {
        Ok(organizations) => organizations,
        Err(error) => {
            let mut cleanup_errors = Vec::new();
            for f in [owner, guest] {
                if let Err(error) = fixture::cleanup(&db, f).await { cleanup_errors.push(error); }
            }
            panic!("could not create organizations: {error}; cleanup errors: {cleanup_errors:?}");
        }
    };
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        for character in [owner.character, guest.character] {
            crate::dao::mailing_lists::tests::set_corporation(&test_db, character, corp).await.unwrap();
        }
        let (client, _peer) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let input = |chars, corporation| SendMail { to_chars: chars, to_list: None, to_corp: corporation, title: "title".into(), body: "body".into(), reply_to: 0, forwarded_from: 0 };
        let personal = send_mail(&test_db, &client.world, owner.character, Some(other), input(vec![guest.character], None)).await.unwrap();
        let self_mail = send_mail(&test_db, &client.world, owner.character, Some(other), input(vec![owner.character], None)).await.unwrap();
        let corporate = send_mail(&test_db, &client.world, owner.character, Some(other), input(vec![guest.character], Some(corp))).await.unwrap();
        let owner_rows = crate::dao::mail::statuses(&test_db, owner.character).await.unwrap();
        let guest_rows = crate::dao::mail::statuses(&test_db, guest.character).await.unwrap();
        let status = |rows: &Vec<crate::dao::mail::Status>, id| { let row = rows.iter().find(|r| r.message_id == id).unwrap(); (row.status_mask, row.label_mask) };
        assert_eq!(status(&owner_rows, personal), (1, 2));
        assert_eq!(status(&guest_rows, personal), (0, 1));
        assert_eq!(status(&owner_rows, self_mail), (0, 3));
        assert_eq!(status(&owner_rows, corporate), (0, 6));
        assert_eq!(status(&guest_rows, corporate), (0, 5));
        assert_eq!(guest_rows.iter().filter(|r| r.message_id == corporate).count(), 1);
        crate::dao::mailing_lists::tests::set_deleted(&test_db, guest.character, true).await.unwrap();
        assert!(matches!(send_mail(&test_db, &client.world, owner.character, None, input(vec![guest.character], None)).await, Err(CallError::User { .. })));
        assert!(matches!(send_mail(&test_db, &client.world, owner.character, Some(corp), input(vec![], Some(other))).await, Err(CallError::User { .. })));
        assert!(matches!(send_mail(&test_db, &client.world, owner.character, None, input(vec![i64::MAX], None)).await, Err(CallError::User { .. })));
    }).await;
    for character in [owner.character, guest.character] { crate::dao::mailing_lists::tests::set_corporation(&db, character, 1).await.unwrap(); }
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    crate::dao::mailing_lists::tests::delete_organizations(&db, corp, other, alliance).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
