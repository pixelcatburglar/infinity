//! Client-facing name and argument validation.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn create_and_welcome_boundaries_through_dispatch() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _peer) = crate::services::tests::client(test_db, owner.character).await;
        let prefix = format!("{}", owner.character);
        let exact = format!("{}{}", prefix, "x".repeat(MAX_LIST_NAME - prefix.len()));
        let args = |name: &str, access, role, cost| vec![Value::unicode(name), access, role, cost];
        let id = list(&mut a, "Create", &args(&exact, Value::Int(1), Value::Int(1), Value::Int(0))).await.unwrap().as_int().unwrap();
        for name in [format!("{exact}x"), " \u{00a0}\u{001c}".to_string()] {
            assert!(list(&mut a, "Create", &args(&name, Value::Int(1), Value::Int(1), Value::Int(0))).await.is_err());
        }
        for (access, role, cost) in [(Value::Bool(true), Value::Int(1), Value::Int(0)), (Value::Int(2), Value::Int(1), Value::Int(0)), (Value::Int(1), Value::Int(2), Value::Int(0)), (Value::Int(1), Value::Int(1), Value::Int(1))] {
            assert!(list(&mut a, "Create", &args("invalid", access, role, cost)).await.is_err());
        }
        assert!(list(&mut a, "SaveWelcomeMail", &[Value::Int(id), Value::unicode(""), Value::unicode("body")]).await.is_err());
        assert!(list(&mut a, "SaveWelcomeMail", &[Value::Int(id), Value::unicode("title"), Value::unicode("")]).await.is_ok());
        assert!(list(&mut a, "SetMembersMuted", &[Value::Int(id), batch(&vec![owner.character; MAX_MEMBER_BATCH + 1])]).await.is_err());
        assert!(list(&mut a, "SetDefaultAccess", &[Value::Int(id), Value::Int(1), Value::Int(1), Value::Int(1)]).await.is_err());
        for method in ["MarkAsReadByList", "MarkAsUnreadByList", "MoveToTrashByList"] {
            assert!(rpc(&mut a, "mailMgr", method, &[Value::Bool(true)]).await.is_err());
        }
        assert!(matches!(list(&mut a, "Join", &[Value::unicode(format!("{exact}x"))]).await, Err(CallError::User { .. })));
        let send = vec![batch(&[owner.character]), Value::None, Value::None, Value::unicode("title"), Value::unicode("body"), Value::Int(0), Value::Int(0)];
        let mut fifty = send.clone(); fifty[0] = batch(&vec![owner.character; MAX_RECIPIENTS]);
        assert!(rpc(&mut a, "mailMgr", "SendMail", &fifty).await.is_ok());
        fifty[0] = batch(&vec![owner.character; MAX_RECIPIENTS + 1]);
        assert!(matches!(rpc(&mut a, "mailMgr", "SendMail", &fifty).await, Err(CallError::User { .. })));
        let entries = Value::Dict(vec![(Value::Int(owner.character), Value::Int(1)); MAX_MEMBER_BATCH]);
        assert!(list(&mut a, "SetEntitiesAccess", &[Value::Int(id), entries]).await.is_ok());
        let title_limit = usize::try_from(crate::config::game().mail.title_max_len).unwrap().min(MAX_TITLE);
        let mut content = send.clone();
        content[3] = Value::unicode("x".repeat(title_limit));
        content[4] = Value::unicode("x".repeat(MAX_BODY));
        assert!(rpc(&mut a, "mailMgr", "SendMail", &content).await.is_ok());
        content[3] = Value::unicode("x".repeat(title_limit + 1));
        assert!(matches!(rpc(&mut a, "mailMgr", "SendMail", &content).await, Err(CallError::User { .. })));
        content[3] = Value::unicode("title");
        content[4] = Value::unicode("x".repeat(MAX_BODY + 1));
        assert!(matches!(rpc(&mut a, "mailMgr", "SendMail", &content).await, Err(CallError::User { .. })));
        for index in [3, 4] {
            let mut bad = send.clone();
            bad[index] = Value::Bytes(vec![0xff]);
            assert!(matches!(rpc(&mut a, "mailMgr", "SendMail", &bad).await, Err(CallError::User { .. })));
        }
        let mut extra = send.clone(); extra.push(Value::None);
        assert!(matches!(rpc(&mut a, "mailMgr", "SendMail", &extra).await, Err(CallError::User { .. })));
        let mut bad = send.clone(); bad[5] = Value::Bool(false);
        assert!(matches!(rpc(&mut a, "mailMgr", "SendMail", &bad).await, Err(CallError::User { .. })));
        let kwargs = Value::Dict(vec![(Value::str("unexpected"), Value::Int(1))]);
        let call = Call { service: "mailMgr", method: "SendMail", args: &send, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(matches!(crate::services::dispatch(&mut a, &call).await, Err(CallError::User { .. })));
        for method in ["MarkAsReadByList", "MarkAsUnreadByList", "MoveToTrashByList"] {
            let args = [Value::Int(id)];
            let call = Call { service: "mailMgr", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert!(matches!(crate::services::dispatch(&mut a, &call).await, Err(CallError::User { .. })));
            assert!(matches!(rpc(&mut a, "mailMgr", method, &[Value::Int(id), Value::None]).await, Err(CallError::User { .. })));
        }
        a.session.apply(vec![("charid", Value::None)]);
        for method in ["MarkAsReadByList", "MarkAsUnreadByList", "MoveToTrashByList"] {
            assert!(matches!(rpc(&mut a, "mailMgr", method, &[Value::Int(id)]).await, Err(CallError::User { .. })));
        }
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character]).await.unwrap();
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn postgres_unicode_folded_name_boundary_through_dispatch() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let name = "ß".repeat(MAX_LIST_NAME);
        let (key, _) = crate::dao::mailing_lists::normalize_name(&test_db, &name).await.unwrap();
        let created = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(1), Value::Int(1), Value::Int(0)]).await;
        if key.chars().count() > MAX_LIST_NAME {
            assert!(matches!(created, Err(CallError::User { .. })));
            assert!(matches!(list(&mut b, "Join", &[Value::unicode(&name)]).await, Err(CallError::User { .. })));
        } else {
            let id = created.unwrap().as_int().unwrap();
            assert_eq!(crate::dao::mailing_lists::get_info(&test_db, id).await.unwrap().unwrap().name_key, key);
            assert!(list(&mut b, "Join", &[Value::unicode(&name)]).await.is_ok());
            assert!(crate::dao::mailing_lists::membership(&test_db, id, guest.character).await.unwrap().is_some());
        }
        let raw = format!(" \\{}", "ß".repeat(MAX_LIST_NAME - 1));
        assert_eq!(raw.chars().count(), MAX_LIST_NAME + 1);
        assert!(matches!(list(&mut b, "Join", &[Value::unicode(&raw)]).await, Err(CallError::User { .. })));
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
