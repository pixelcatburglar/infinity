//! Real corporation and alliance ACL precedence.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn organization_precedence_uses_current_character_corporation() {
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
        crate::dao::mailing_lists::tests::set_corporation(&test_db, guest.character, corp).await.unwrap();
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let name = format!("organization {}", owner.character);
        let id = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(0), Value::Int(1), Value::Int(0)]).await.unwrap().as_int().unwrap();
        let set = |entity, access| [Value::Int(id), Value::Dict(vec![(Value::Int(entity), Value::Int(access))])];
        let join = [Value::unicode(&name)];
        let leave = [Value::Int(id)];
        list(&mut a, "SetEntitiesAccess", &set(alliance, 1)).await.unwrap();
        list(&mut b, "Join", &join).await.unwrap();
        list(&mut b, "Leave", &leave).await.unwrap();
        list(&mut a, "SetEntitiesAccess", &set(corp, 0)).await.unwrap();
        assert!(matches!(list(&mut b, "Join", &join).await, Err(CallError::User { .. })));
        list(&mut a, "SetEntitiesAccess", &set(guest.character, 1)).await.unwrap();
        list(&mut b, "Join", &join).await.unwrap();
        list(&mut a, "ClearEntityAccess", &[Value::Int(id), Value::Int(guest.character)]).await.unwrap();
        assert!(crate::dao::mailing_lists::membership(&test_db, id, guest.character).await.unwrap().is_some());
        list(&mut b, "Leave", &leave).await.unwrap();
        list(&mut a, "SetEntitiesAccess", &set(corp, 1)).await.unwrap();
        list(&mut a, "SetEntitiesAccess", &set(guest.character, 0)).await.unwrap();
        assert!(matches!(list(&mut b, "Join", &join).await, Err(CallError::User { .. })));
        list(&mut a, "ClearEntityAccess", &[Value::Int(id), Value::Int(guest.character)]).await.unwrap();
        b.session.apply(vec![("corpid", Value::Int(corp))]);
        crate::dao::mailing_lists::tests::set_corporation(&test_db, guest.character, other).await.unwrap();
        assert!(matches!(list(&mut b, "Join", &join).await, Err(CallError::User { .. })));
        list(&mut a, "SetEntitiesAccess", &set(other, 1)).await.unwrap();
        list(&mut b, "Join", &join).await.unwrap();
    }).await;
    crate::dao::mailing_lists::tests::set_corporation(&db, guest.character, 1).await.unwrap();
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    crate::dao::mailing_lists::tests::delete_organizations(&db, corp, other, alliance).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
