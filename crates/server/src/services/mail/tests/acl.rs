//! Character ACL overrides and non-retroactive membership policy.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn character_override_muted_and_existing_membership() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let name = format!("acl {}", owner.character);
        let id = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(0), Value::Int(0), Value::Int(0)]).await.unwrap().as_int().unwrap();
        let key = Value::Int(id);
        assert!(list(&mut b, "Join", &[Value::unicode(&name)]).await.is_err());
        list(&mut a, "SetEntitiesAccess", &[key.clone(), Value::Dict(vec![(Value::Int(guest.character), Value::Int(1))])]).await.unwrap();
        let view = list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        assert_eq!(crate::services::value_field(&view, "isMuted"), Some(&Value::Bool(true)));
        let send = [batch(&[]), key.clone(), Value::None, Value::unicode("title"), Value::unicode("body"), Value::Int(0), Value::Int(0)];
        assert!(rpc(&mut b, "mailMgr", "SendMail", &send).await.is_err());
        list(&mut a, "SetEntitiesAccess", &[key.clone(), Value::Dict(vec![(Value::Int(guest.character), Value::Int(0))])]).await.unwrap();
        assert_eq!(crate::dao::mailing_lists::membership(&test_db, id, guest.character).await.unwrap(), Some(0));
        rpc(&mut a, "mailMgr", "SendMail", &send).await.unwrap();
        assert_eq!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().len(), 1);
        list(&mut b, "Leave", &[key.clone()]).await.unwrap();
        assert!(list(&mut b, "Join", &[Value::unicode(&name)]).await.is_err());
        list(&mut a, "ClearEntityAccess", &[key.clone(), Value::Int(guest.character)]).await.unwrap();
        list(&mut a, "SetDefaultAccess", &[key, Value::Int(1), Value::Int(1), Value::Int(0)]).await.unwrap();
        assert!(list(&mut b, "Join", &[Value::unicode(&name)]).await.is_ok());
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
