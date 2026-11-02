use super::*;

#[test]
fn context_fields_and_session_expire_on_drop() {
    let lua = Lua::new();
    let live = Arc::new(AtomicBool::new(true));
    let guard = InvocationGuard { live: live.clone(), hook_thread: Arc::new(Mutex::new(None)) };
    let (tx, _rx) = mpsc::unbounded_channel();
    let char_id = Arc::new(AtomicI64::new(7));
    let ctx = Context { live, tx, user_id: 1, char_id: char_id.clone(), role: GM_ROLES };
    lua.globals().set("ctx", ctx).unwrap();
    assert!(lua.load("return ctx.user_id == 1 and ctx.char_id == 7 and ctx:is_gm()").eval::<bool>().unwrap());
    char_id.store(8, Ordering::Release);
    assert_eq!(lua.load("return ctx.char_id").eval::<i64>().unwrap(), 8);
    lua.load("saved_session = ctx.session").exec().unwrap();
    drop(guard);
    assert!(lua.load("return ctx.user_id").eval::<i64>().is_err());
    assert!(lua.load("return ctx:is_gm()").eval::<bool>().is_err());
    assert!(lua.load("return ctx.session").eval::<mlua::Value>().is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn invocation_deadline_invalidates_context_and_vm_remains_usable() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let cfg = Arc::new(crate::config::Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let conn = crate::conn::Conn::new(stream, address);
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
    let chat = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    let mut client = Client::new(conn, cfg, db, sde, chat, world, login);
    let lua = Lua::new();
    crate::lua::value::register(&lua, &lua.globals()).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let update = crate::lua::vm::budgeted_invocation(&lua, lua.load("local ctx = ...; ctx:change_session({charid=eve.int(7)}); return ctx.char_id").into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
    let result = match invoke(&mut client, &lua, update, vec![], deadline).await { Ok(value) => value, Err(_) => panic!("session change failed") };
    assert!(matches!(result.front(), Some(mlua::Value::Integer(7))));
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = dropped.clone();
    lua.globals().set("wait_forever", lua.create_async_function(move |_, ()| {
        let marker = marker.clone();
        async move {
            let _guard = Dropped(marker);
            std::future::pending::<mlua::Result<()>>().await
        }
    }).unwrap()).unwrap();
    let function = lua.load("local ctx = ...; saved = ctx; saved_session = ctx.session; wait_forever(); resumed = true").into_function().unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(20);
    let invocation = crate::lua::vm::budgeted_invocation(&lua, function, &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
    assert!(invoke(&mut client, &lua, invocation, vec![], deadline).await.is_err());
    assert!(lua.globals().get::<Option<bool>>("resumed").unwrap().is_none());
    assert!(dropped.load(Ordering::Acquire), "pending host future must be dropped before returning");
    assert!(lua.load("return saved.user_id").eval::<i64>().is_err());
    let saved_session: mlua::AnyUserData = lua.globals().get("saved_session").unwrap();
    lua.globals().set("saved_session", saved_session).unwrap();
    assert!(lua.load("return saved_session:get('charid')").exec_async().await.is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let config = crate::config::lua::LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..Default::default() };
    let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load("while true do end").into_function().unwrap(), &config, deadline.into_std()).unwrap();
    assert!(invoke(&mut client, &lua, invocation, vec![], deadline).await.is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load("local ctx = ...; return ctx.user_id").into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
    let result = match invoke(&mut client, &lua, invocation, vec![], deadline).await { Ok(value) => value, Err(_) => panic!("later invocation failed") };
    assert!(matches!(result.front(), Some(mlua::Value::Integer(1))));
    drop(peer);
}

#[tokio::test]
async fn mutations_reject_expired_context_without_enqueueing() {
    let lua = Lua::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let ctx = lua.create_userdata(Context {
        live: Arc::new(AtomicBool::new(false)),
        tx,
        user_id: 1,
        char_id: Arc::new(AtomicI64::new(1)),
        role: 0,
    }).unwrap();
    let result = mutate(ctx, Mutation::Delete { item: 1 }).await;
    assert!(result.unwrap_err().to_string().contains("expired"));
    assert!(rx.try_recv().is_err());
}

#[path = "../../../dao/items/tests.rs"]
mod mutation_fixture;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn real_mutations_commit_and_cleanup_fixture_rows() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let before = mutation_fixture::fingerprint(&db).await.unwrap();
    let fixture = mutation_fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    // A task boundary ensures cleanup also runs when an assertion or Lua call panics.
    let result = tokio::spawn(async move {
        let sde = Arc::new(crate::sde::Sde::load(&test_db).await.unwrap());
        let type_id = sde.types.iter().find(|t| t.type_id > 0).expect("SDE must contain a type").type_id;
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let (tx, mut notifications) = mpsc::unbounded_channel();
        world.register_pilot(fixture.chars[0], tx);
        let (tx, mut recipient_notifications) = mpsc::unbounded_channel();
        world.register_pilot(fixture.chars[1], tx);
        let cfg = Arc::new(crate::config::Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "test".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde.clone(), chat, world, login);
        client.change_session(vec![("charid", Value::Int(fixture.chars[0]))]);
        let lua = Lua::new();
        crate::lua::value::register(&lua, &lua.globals()).unwrap();
        super::super::dao::register(&lua, &lua.globals(), &test_db, &sde).unwrap();
        lua.globals().set("me", fixture.chars[0]).unwrap();
        lua.globals().set("recipient", fixture.chars[1]).unwrap();
        lua.globals().set("other", fixture.chars[2]).unwrap();
        lua.globals().set("type_id", type_id).unwrap();
        lua.globals().set("flag", crate::consts::FLAG_HANGAR).unwrap();
        let script = r#"
            local ctx = ...
            assert(dao.wallets.balance(me) == 0)
            assert(dao.wallets.credit(ctx, me, 20) == nil)
            assert(dao.wallets.debit(ctx, me, 7) == nil)
            assert(not pcall(dao.wallets.debit, ctx, me, 100))
            assert(dao.wallets.balance(me) == 13)
            assert(dao.standings.set(ctx, me, other, 2.5) == nil)
            assert(not pcall(dao.standings.set, ctx, me, other, 11))
            assert(not pcall(dao.standings.set, ctx, me, other, 0/0))
            assert(dao.standings.get(me, other) == 2.5)
            assert(not pcall(dao.mail.send, ctx, {}, "title", "body"))
            assert(not pcall(dao.mail.send, ctx, {recipient, other}, string.rep("x", 4096), "body"))
            local message = dao.mail.send(ctx, {recipient, other}, "x", "body")
            assert(message and message > 0)
            assert(dao.items.create(ctx, me, me, flag, type_id, 2) == nil)
            assert(dao.items.create(ctx, me, me, flag, type_id, 3) == nil)
            local owned = dao.items.by_owner(me)
            local located = dao.items.by_location(me)
            assert(#owned == 1 and #located == 1)
            assert(owned[1].item_id == located[1].item_id and owned[1].quantity == 5)
            assert(dao.items.create(ctx, other, other, flag, type_id, 1) == nil)
            local foreign = dao.items.by_owner(other)[1].item_id
            assert(dao.items.delete(ctx, foreign) == nil)
            assert(#dao.items.by_owner(other) == 1)
            ctx:change_session({shipid = eve.int(owned[1].item_id)})
            assert(dao.items.delete(ctx, owned[1].item_id) == nil)
            assert(#dao.items.by_owner(me) == 1)
            assert(not pcall(dao.items.move, ctx, owned[1].item_id, other, nil, nil))
            ctx:change_session({shipid = eve.int(0)})
            local moved = dao.items.move(ctx, owned[1].item_id, other, nil, 2)
            assert(moved and moved ~= owned[1].item_id)
            assert(#dao.items.by_owner(me) == 2)
            assert(dao.items.delete(ctx, moved) == nil)
            assert(dao.items.delete(ctx, owned[1].item_id) == nil)
            assert(#dao.items.by_owner(me) == 0)
            return message
        "#;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load(script).into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
        let values = invoke(&mut client, &lua, invocation, vec![], deadline).await.unwrap_or_else(|error| panic!("mutation invocation failed: {error:?}"));
        let message = match values.front() { Some(mlua::Value::Integer(id)) if *id > 0 => *id, _ => panic!("missing message id") };
        let headers = crate::dao::mail::headers(&test_db, fixture.chars[0], &vec![message]).await.unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].title, "x");
        assert_eq!(headers[0].to_character_ids, format!("{},{}", fixture.chars[1], fixture.chars[2]));
        for (character, status, label) in [(fixture.chars[0], 1, 2), (fixture.chars[1], 0, 1), (fixture.chars[2], 0, 1)] {
            let rows = crate::dao::mail::statuses(&test_db, character).await.unwrap();
            assert!(rows.iter().any(|row| row.message_id == message && row.status_mask == status && row.label_mask == label));
        }
        assert_eq!(notifications.try_recv().unwrap().0, "OnAccountChange");
        assert_eq!(notifications.try_recv().unwrap().0, "OnAccountChange");
        assert_eq!(notifications.try_recv().unwrap().0, "OnStandingSet");
        assert!(notifications.try_recv().is_err(), "refused mutations must not notify");
        assert_eq!(recipient_notifications.try_recv().unwrap().0, "OnMailSent");
        let rollback_world = client.world.clone();
        let rollback: Result<(), crate::services::CallError> = test_db.transaction(
            |tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
                crate::services::wallets::credit(tx, outbox, fixture.chars[0], 5.0).await?;
                Err(crate::services::CallError::Internal(anyhow::anyhow!("fixture rollback")))
            }),
            |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &rollback_world) },
        ).await;
        assert!(rollback.is_err());
        assert_eq!(match crate::services::wallets::balance(&test_db, fixture.chars[0]).await { Ok(value) => value, Err(_) => panic!("wallet balance failed") }, 13.0);
        assert!(notifications.try_recv().is_err(), "rolled-back credit must not notify");
        assert!(!client.wait_pumping(-1, 0).await.unwrap());
        let mut item_notices = 0;
        while let Ok(Ok(Some(frame))) = tokio::time::timeout(std::time::Duration::from_millis(50), crate::framing::read_frame(&mut peer)).await {
            let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            if let crate::macho::Address::Broadcast { broadcast_id, .. } = packet.destination {
                if broadcast_id.as_text().as_deref() == Some("OnItemChange") { item_notices += 1; }
            }
        }
        assert_eq!(item_notices, 7, "only committed creates, split moves and owned deletes notify");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let gate_db = test_db.clone();
        let gate = tokio::spawn(async move {
            mutation_fixture::hold_character(&gate_db, fixture.chars[0], ready_tx, release_rx).await.unwrap();
        });
        ready_rx.await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let monitor_db = test_db.clone();
        let monitor = tokio::spawn(async move {
            loop {
                if mutation_fixture::wallet_update_waiting(&monitor_db).await.unwrap() { break; }
                assert!(tokio::time::Instant::now() < deadline, "wallet transaction did not reach the row lock");
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tokio::time::sleep_until(deadline + std::time::Duration::from_millis(50)).await;
            release_tx.send(()).unwrap();
        });
        let function = lua.load("local ctx = ...; dao.wallets.credit(ctx, me, 1); shield_resumed = true").into_function().unwrap();
        let invocation = crate::lua::vm::budgeted_invocation(&lua, function, &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
        assert!(invoke(&mut client, &lua, invocation, vec![], deadline).await.is_err());
        monitor.await.unwrap();
        gate.await.unwrap();
        assert!(lua.globals().get::<Option<bool>>("shield_resumed").unwrap().is_none());
        assert_eq!(match crate::services::wallets::balance(&test_db, fixture.chars[0]).await { Ok(value) => value, Err(_) => panic!("wallet balance failed") }, 14.0);
        assert_eq!(notifications.try_recv().unwrap().0, "OnAccountChange");
        assert!(notifications.try_recv().is_err());
    }).await;
    mutation_fixture::cleanup(&db, fixture).await.unwrap();
    assert_eq!(mutation_fixture::fingerprint(&db).await.unwrap(), before);
    result.expect("mutation fixture failed");
}
