use super::*;

#[test]
fn callback_user_error_preserves_message_and_dictionary() {
    let lua = Lua::new();
    let dict = vec![
        ("item".to_owned(), Value::Int(42)),
        ("detail".to_owned(), Value::unicode("refused")),
    ];
    let expected = dict.clone();
    let callback = lua.create_function(move |_, ()| -> mlua::Result<()> {
        Err(lua_error(CallError::User { msg: "Refused".into(), dict: dict.clone() }))
    }).unwrap();
    lua.globals().set("refuse", callback).unwrap();
    let error = lua.load("refuse()").exec().unwrap_err();
    assert!(matches!(&error, mlua::Error::CallbackError { .. }));
    match call_error(error) {
        CallError::User { msg, dict } => {
            assert_eq!(msg, "Refused");
            assert_eq!(dict, expected);
        }
        _ => panic!("expected user refusal"),
    }
}

#[test]
fn plain_lua_runtime_error_remains_internal() {
    let lua = Lua::new();
    let error = lua.load("error('runtime failure')").exec().unwrap_err();
    let original = error.to_string();
    match call_error(error) {
        CallError::Internal(error) => {
            let lua_error = error.downcast_ref::<mlua::Error>().unwrap();
            assert!(matches!(lua_error, mlua::Error::RuntimeError(_)));
            assert_eq!(lua_error.to_string(), original);
        }
        _ => panic!("expected internal error"),
    }
}

#[test]
fn exact_amount_is_unclamped_numeric_in_lua() {
    let lua = Lua::new();
    let amount = crate::isk::IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
    lua.globals().set("balance", amount).unwrap();
    assert!(lua.load("return type(balance) == 'number' and balance > 1e30").eval::<bool>().unwrap());
    assert_eq!(amount.decimal(), "9999999999999999999999999999999999.99");
}

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
async fn retained_context_cannot_request_snapshot_after_expiry() {
    let lua = Lua::new();
    let live = Arc::new(AtomicBool::new(true));
    let guard = InvocationGuard { live: live.clone(), hook_thread: Arc::new(Mutex::new(None)) };
    let (tx, mut rx) = mpsc::unbounded_channel();
    lua.globals().set("ctx", Context { live, tx, user_id: 1, char_id: Arc::new(AtomicI64::new(7)), role: 0 }).unwrap();
    drop(guard);
    assert!(lua.load("return ctx:content_snapshot('sovereignty')").exec_async().await.is_err());
    assert!(rx.try_recv().is_err());
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
    let result = match invoke(&mut client, &lua, update, vec![], deadline, None, None).await { Ok(value) => value, Err(_) => panic!("session change failed") };
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
    assert!(invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.is_err());
    assert!(lua.globals().get::<Option<bool>>("resumed").unwrap().is_none());
    assert!(dropped.load(Ordering::Acquire), "pending host future must be dropped before returning");
    assert!(lua.load("return saved.user_id").eval::<i64>().is_err());
    let saved_session: mlua::AnyUserData = lua.globals().get("saved_session").unwrap();
    lua.globals().set("saved_session", saved_session).unwrap();
    assert!(lua.load("return saved_session:get('charid')").exec_async().await.is_err());
    assert!(lua.load("return saved:content_snapshot('sovereignty')").exec_async().await.is_err());
    assert!(lua.load("return saved:content_snapshot('ship_adaption')").exec_async().await.is_err());
    assert!(lua.load("return saved:content_commit('ship_adaption', 0)").exec_async().await.is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let config = crate::config::lua::LuaConfig { call_budget_instructions: 1000, instruction_hook_interval: 100, ..Default::default() };
    let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load("while true do end").into_function().unwrap(), &config, deadline.into_std()).unwrap();
    assert!(invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.is_err());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load("local ctx = ...; return ctx.user_id").into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
    let result = match invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await { Ok(value) => value, Err(_) => panic!("later invocation failed") };
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
    let fixture = mutation_fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    // A task boundary ensures cleanup also runs when an assertion or Lua call panics.
    let result = tokio::spawn(async move {
        let sde = Arc::new(crate::sde::Sde::load(&test_db).await.unwrap());
        let station = sde.map.stations().find(|s| s.solar_system_id.is_some()).expect("SDE station");
        let (station_id, system_id) = (station.station_id, station.solar_system_id.unwrap());
        let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).expect("published SDE ship").type_id;
        let type_id = sde.types.iter().find(|t| t.published == Some(true) && sde.is_tradable(t.type_id) && t.volume == Some(0.0)).expect("published zero-volume tradable SDE type").type_id;
        let source = crate::dao::items::creation::rookie_ship(&test_db, ship_type, fixture.chars[0], station_id, infinity_core::consts::FLAG_HANGAR).await.unwrap();
        let destination = crate::dao::items::creation::rookie_ship(&test_db, ship_type, fixture.chars[0], station_id, infinity_core::consts::FLAG_HANGAR).await.unwrap();
        let box_id = crate::dao::items::creation::rookie_ship(&test_db, ship_type, fixture.chars[2], station_id, infinity_core::consts::FLAG_HANGAR).await.unwrap();
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let (sender_tx, mut notifications) = mpsc::unbounded_channel();
        let (tx, mut recipient_notifications) = mpsc::unbounded_channel();
        world.register_pilot(fixture.chars[1], tx);
        let cfg = Arc::new(crate::config::Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "test".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde.clone(), chat, world, login);
        client.notify_tx = sender_tx;
        client.change_session(vec![("charid", Value::Int(fixture.chars[0])), ("stationid", Value::Int(station_id)), ("solarsystemid", Value::Int(system_id)), ("solarsystemid2", Value::Int(system_id))]);
        let lua = Lua::new();
        crate::lua::value::register(&lua, &lua.globals()).unwrap();
        super::super::dao::register(&lua, &lua.globals(), &test_db, &sde).unwrap();
        lua.globals().set("me", fixture.chars[0]).unwrap();
        lua.globals().set("recipient", fixture.chars[1]).unwrap();
        lua.globals().set("other", fixture.chars[2]).unwrap();
        test_db.execute("UPDATE game.characters SET balance=9999999999999999999999999999999999.99 WHERE character_id=$1", &[&fixture.chars[2]]).await.unwrap();
        assert!(lua.load("local value = dao.wallets.balance(other); return type(value) == 'number' and value > 1e30").eval_async::<bool>().await.unwrap());
        assert_eq!(crate::services::wallets::balance(&test_db, fixture.chars[2]).await.unwrap().decimal(), "9999999999999999999999999999999999.99");
        lua.globals().set("type_id", type_id).unwrap();
        lua.globals().set("source", source).unwrap();
        lua.globals().set("destination", destination).unwrap();
        lua.globals().set("box_id", box_id).unwrap();
        lua.globals().set("flag", infinity_core::consts::FLAG_CARGO).unwrap();
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
            assert(dao.items.create(ctx, me, source, flag, type_id, 9007199254740993) == nil)
            assert(dao.items.create(ctx, me, source, flag, type_id, 3) == nil)
            local owned = dao.items.by_owner(me)
            local located = dao.items.by_location(source)
            assert(#owned == 3 and #located == 1)
            local stack = located[1].item_id
            assert(located[1].quantity == 9007199254740996 and math.type(located[1].quantity) == "integer")
            assert(dao.items.create(ctx, other, box_id, flag, type_id, 1) == nil)
            local foreign = dao.items.by_location(box_id)[1].item_id
            return message, stack, foreign
        "#;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load(script).into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
        let values = invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.unwrap_or_else(|error| panic!("mutation invocation failed: {error:?}"));
        let message = match values.front() { Some(mlua::Value::Integer(id)) if *id > 0 => *id, _ => panic!("missing message id") };
        let stack = match values.get(1) { Some(mlua::Value::Integer(id)) => *id, _ => panic!("missing stack id") };
        let foreign = match values.get(2) { Some(mlua::Value::Integer(id)) => *id, _ => panic!("missing foreign id") };
        lua.globals().set("stack", stack).unwrap();
        lua.globals().set("foreign", foreign).unwrap();
        assert_eq!(notifications.try_recv().unwrap().0, "OnAccountChange");
        assert_eq!(notifications.try_recv().unwrap().0, "OnAccountChange");
        assert_eq!(notifications.try_recv().unwrap().0, "OnStandingSet");
        assert!(notifications.try_recv().is_err());
        let item_count = client.outbox.iter().filter(|packet| matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnItemChange"))).count();
        assert_eq!(item_count, 3);
        let script = r#"
            local ctx = ...
            assert(not pcall(dao.items.delete, ctx, foreign))
            local foreign_items = dao.items.by_location(box_id)
            assert(#foreign_items == 1 and foreign_items[1].item_id == foreign)
            ctx:change_session({shipid = eve.int(stack)})
            assert(not pcall(dao.items.delete, ctx, stack))
            local active_ship_items = dao.items.by_owner(me)
            assert(#active_ship_items == 3)
            local active_ship_location = dao.items.by_location(source)
            assert(#active_ship_location == 1 and active_ship_location[1].item_id == stack and active_ship_location[1].quantity == 9007199254740996)
            assert(not pcall(dao.items.move, ctx, stack, other, nil, nil))
            assert(not pcall(dao.items.move, ctx, stack, destination, nil, nil))
            ctx:change_session({shipid = eve.int(0)})
        "#;
        let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load(script).into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
        invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.unwrap_or_else(|error| panic!("refusal invocation failed: {error:?}"));
        assert!(notifications.try_recv().is_err(), "refused mutations must not notify");
        assert_eq!(client.outbox.iter().filter(|packet| matches!(&packet.destination, crate::macho::Address::Broadcast { broadcast_id, .. } if broadcast_id.as_text().as_deref() == Some("OnItemChange"))).count(), item_count, "refused mutations must not notify directly");
        let script = r#"
            local ctx = ...
            local moved = dao.items.move(ctx, stack, destination, nil, 9007199254740993)
            assert(moved and moved ~= stack)
            assert(#dao.items.by_owner(me) == 4)
            local remaining = dao.items.by_location(source)
            local transferred = dao.items.by_location(destination)
            assert(#remaining == 1 and remaining[1].item_id == stack and remaining[1].quantity == 3)
            assert(#transferred == 1 and transferred[1].item_id == moved and transferred[1].quantity == 9007199254740993)
            assert(dao.items.delete(ctx, moved) == nil)
            assert(dao.items.delete(ctx, stack) == nil)
            assert(#dao.items.by_owner(me) == 2)
            assert(#dao.items.by_location(source) == 0 and #dao.items.by_location(destination) == 0)
            local hulls = dao.items.by_owner(me)
            assert(#hulls == 2)
            assert((hulls[1].item_id == source and hulls[2].item_id == destination) or (hulls[1].item_id == destination and hulls[2].item_id == source))
            return moved
        "#;
        let invocation = crate::lua::vm::budgeted_invocation(&lua, lua.load(script).into_function().unwrap(), &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
        let values = invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.unwrap_or_else(|error| panic!("mutation invocation failed: {error:?}"));
        let moved = match values.front() { Some(mlua::Value::Integer(id)) => *id, _ => panic!("missing moved id") };
        let headers = crate::dao::mail::headers(&test_db, fixture.chars[0], &vec![message]).await.unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].title, "x");
        assert_eq!(headers[0].to_character_ids, format!("{},{}", fixture.chars[1], fixture.chars[2]));
        for (character, status, label) in [(fixture.chars[0], 1, 2), (fixture.chars[1], 0, 1), (fixture.chars[2], 0, 1)] {
            let rows = crate::dao::mail::statuses(&test_db, character).await.unwrap();
            assert!(rows.iter().any(|row| row.message_id == message && row.status_mask == status && row.label_mask == label));
        }
        let descriptor = crate::services::item_descriptor();
        for (id, location, quantity, change) in [
            (stack, source, 3, Value::Dict(vec![(Value::Int(infinity_core::consts::IX_STACK_SIZE), Value::Int(infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT))])),
            (moved, destination, infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT, Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(source)), (Value::Int(infinity_core::consts::IX_FLAG), Value::Int(i64::from(infinity_core::consts::FLAG_CARGO)))])),
            (moved, infinity_core::consts::LOCATION_JUNKYARD, infinity_core::consts::CLIENT_MAX_SHORT_AMOUNT, Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(destination))])),
            (stack, infinity_core::consts::LOCATION_JUNKYARD, 3, Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(source))])),
        ] {
            let (event, args) = notifications.try_recv().unwrap();
            assert_eq!(event, "OnItemChange");
            assert_eq!(descriptor.get(&args[0], "itemID"), Some(Value::Int(id)));
            assert_eq!(descriptor.get(&args[0], "locationID"), Some(Value::Int(location)));
            assert_eq!(descriptor.get(&args[0], "quantity"), Some(Value::Int(quantity)));
            assert_eq!(args[1], change);
            assert_eq!(args[2], Value::None);
        }
        assert!(notifications.try_recv().is_err());
        assert_eq!(recipient_notifications.try_recv().unwrap().0, "OnMailSent");
        let rollback_world = client.world.clone();
        let rollback: Result<(), crate::services::CallError> = test_db.transaction(
            |tx, outbox: &mut crate::db::outbox::Outbox| Box::pin(async move {
                crate::services::wallets::credit(tx, outbox, fixture.chars[0], crate::isk::IskAmount::from_cents(500).unwrap()).await?;
                Err(crate::services::CallError::Internal(anyhow::anyhow!("fixture rollback")))
            }),
            |value, outbox| async move { crate::db::outbox::dispatch_world(value, outbox, &rollback_world) },
        ).await;
        assert!(rollback.is_err());
        assert_eq!(match crate::services::wallets::balance(&test_db, fixture.chars[0]).await { Ok(value) => value, Err(_) => panic!("wallet balance failed") }, crate::isk::IskAmount::from_cents(1300).unwrap());
        assert!(notifications.try_recv().is_err(), "rolled-back credit must not notify");
        assert!(!client.wait_pumping(-1, 0).await.unwrap());
        let mut item_notices = 0;
        while let Ok(Ok(Some(frame))) = tokio::time::timeout(std::time::Duration::from_millis(50), crate::framing::read_frame(&mut peer)).await {
            let packet = crate::macho::Packet::from_value(&marshal::loads(&frame.payload).unwrap()).unwrap();
            if let crate::macho::Address::Broadcast { broadcast_id, .. } = packet.destination {
                if broadcast_id.as_text().as_deref() == Some("OnItemChange") { item_notices += 1; }
            }
        }
        assert_eq!(item_notices, 3, "only committed creates notify directly");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let gate_db = test_db.clone();
        let gate = tokio::spawn(async move {
            mutation_fixture::hold_character(&gate_db, fixture.chars[0], ready_tx, release_rx).await.unwrap();
        });
        let blocker_pid = ready_rx.await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let monitor_db = test_db.clone();
        let monitor = tokio::spawn(async move {
            loop {
                if mutation_fixture::wallet_update_waiting(&monitor_db, blocker_pid).await.unwrap() { break; }
                assert!(tokio::time::Instant::now() < deadline, "wallet transaction did not reach the row lock");
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tokio::time::sleep_until(deadline + std::time::Duration::from_millis(50)).await;
            release_tx.send(()).unwrap();
        });
        let function = lua.load("local ctx = ...; dao.wallets.credit(ctx, me, 1); shield_resumed = true").into_function().unwrap();
        let invocation = crate::lua::vm::budgeted_invocation(&lua, function, &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
        assert!(invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.is_err());
        monitor.await.unwrap();
        gate.await.unwrap();
        assert!(lua.globals().get::<Option<bool>>("shield_resumed").unwrap().is_none());
        assert_eq!(match crate::services::wallets::balance(&test_db, fixture.chars[0]).await { Ok(value) => value, Err(_) => panic!("wallet balance failed") }, crate::isk::IskAmount::from_cents(1400).unwrap());
        assert_eq!(notifications.try_recv().unwrap().0, "OnAccountChange");
        assert!(notifications.try_recv().is_err());
    }).await;
    mutation_fixture::cleanup(&db, fixture).await.unwrap();
    let remaining: i64 = db.query_one("SELECT (SELECT count(*) FROM game.accounts WHERE user_id=$1) + (SELECT count(*) FROM game.characters WHERE character_id=ANY($2) OR user_id=$1) + (SELECT count(*) FROM game.items WHERE owner_id=ANY($2) OR location_id=ANY($2)) AS n", &[&fixture.account, &fixture.chars.to_vec()]).await.unwrap().get("n");
    assert_eq!(remaining, 0);
    result.expect("mutation fixture failed");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn sovereignty_snapshot_rechecks_membership_and_returns_wire() {
    use crate::services::sovereignty::queries::Query;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = mutation_fixture::create(&db).await.unwrap();
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('lua-sov-' || nextval('game.corporations_corporation_id_seq')::text, 'LS' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&fixture.chars[0]]).await.unwrap().get("corporation_id");
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('lua-sov-' || nextval('game.alliances_alliance_id_seq')::text, 'LS' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp, &fixture.chars[0]]).await.unwrap().get("alliance_id");
    db.execute("UPDATE game.corporations SET alliance_id=$2 WHERE corporation_id=$1", &[&corp, &alliance]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$2 WHERE character_id=$1", &[&fixture.chars[0], &corp]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = Arc::new(crate::sde::Sde::load(&test_db).await.unwrap());
        let world = crate::space::World::new(test_db.clone(), sde.clone(), 1);
        let cfg = Arc::new(crate::config::Config::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
        let chat = crate::chat::Hub::new(&cfg.chat.domain, test_db.clone(), world.clone());
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde, chat, world, login);
        let lua = Lua::new();
        crate::lua::value::register(&lua, &lua.globals()).unwrap();
        lua.globals().set("other", fixture.chars[1]).unwrap();
        let run = |source: &str| {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            let function = lua.load(source).into_function().unwrap();
            let invocation = crate::lua::vm::budgeted_invocation(&lua, function, &crate::config::lua::LuaConfig::default(), deadline.into_std()).unwrap();
            (invocation, deadline)
        };
        let (invocation, deadline) = run("local ctx = ...; saved = ctx; ctx:change_session({charid=eve.int(other)}); return ctx:content_snapshot('sovereignty')");
        let mut value = invoke(&mut client, &lua, invocation, vec![], deadline, Some((fixture.chars[0], Query::CapitalSystemInfo { alliance_id: alliance })), None).await.unwrap();
        let wire = from_lua(value.pop_front().unwrap()).unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
        assert!(lua.load("return saved:content_snapshot('sovereignty')").exec_async().await.is_err());
        let (invocation, deadline) = run("local ctx = ...; return ctx:content_commit('sovereignty', 0)");
        assert!(invoke(&mut client, &lua, invocation, vec![], deadline, Some((fixture.chars[0], Query::CapitalSystemInfo { alliance_id: alliance })), None).await.is_err());
        lua.globals().set("foreign", alliance + 1).unwrap();
        let (invocation, deadline) = run("local ctx = ...; return ctx:bind('allianceRegistry', eve.tuple{eve.int(foreign), eve.int(1)})");
        invoke(&mut client, &lua, invocation, vec![], deadline, None, None).await.unwrap();
        let binding = client.bound.values().find_map(|(service, params)| (service == "allianceRegistry").then_some(params.clone())).unwrap();
        let kwargs = Value::Dict(vec![]);
        let call = crate::services::Call { service: "allianceRegistry", method: "GetCapitalSystemInfo", args: &[], kwargs: &kwargs, bound: true, bind_params: Some(&binding) };
        let query = crate::lua::sovereignty_query(&call, Some(fixture.chars[0]), None).unwrap();
        let (invocation, deadline) = run("local ctx = ...; return ctx:content_snapshot('sovereignty')");
        assert!(invoke(&mut client, &lua, invocation, vec![], deadline, Some((fixture.chars[0], query)), None).await.is_err());
        let (invocation, deadline) = run("local ctx = ...; return ctx:content_snapshot('sovereignty')");
        assert!(invoke(&mut client, &lua, invocation, vec![], deadline, Some((fixture.chars[0], Query::CapitalSystemInfo { alliance_id: alliance + 1 })), None).await.is_err());
        test_db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
        let (invocation, deadline) = run("local ctx = ...; return ctx:content_snapshot('sovereignty')");
        assert!(invoke(&mut client, &lua, invocation, vec![], deadline, Some((fixture.chars[0], Query::CapitalSystemInfo { alliance_id: alliance })), None).await.is_err());
    }).await;
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&fixture.chars[0]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    mutation_fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn expired_live_context_cannot_submit_owned_adaptation() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), character.character).await;
    client.login.user_id = character.account;
    let fixture = crate::lua::tests::ship_adaption::fixture::create(&db, client.sde.clone(), client.world.clone()).await;
    client.session.apply(vec![("stationid", Value::Int(fixture.station))]);
    db.execute("UPDATE game.characters SET station_id=$2 WHERE character_id=$1", &[&character.character, &fixture.station]).await.unwrap();
    let item = crate::dao::items::transfers::insert(&db, fixture.injector, character.character, fixture.station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let (sender, mut notices) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = sender.clone();
    client.world.register_pilot(character.character, sender);
    let pin = crate::lua::content::PinnedPolicy::pin(fixture.host.snapshot(), "ship_adaption").unwrap();
    let identity = crate::services::identity::Identity::of_client(&client);
    let live = AtomicBool::new(true);
    let deadline = tokio::time::Instant::now() - std::time::Duration::from_secs(1);
    let metadata = AdaptionInvocation {
        host: &fixture.host, pin: crate::lua::content::PinnedPolicy::pin(fixture.host.snapshot(), "ship_adaption").unwrap(),
        request: crate::services::ship_adaption::model::Request::Inject { item, quantity: 1 }, identity, user: character.account,
    };
    let before = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert!(matches!(run_adaption(&mut client, metadata, before, &live, deadline).await, Err(CallError::Internal(_))));
    let plan = crate::lua::tests::ship_adaption::fixture::inject_plan(&pin, &fixture.sde, fixture.injector, 1);
    let result = submit_adaption(&client, fixture.host.clone(), pin, identity, character.account, 0,
        plan, Some(item), &live, deadline).await;
    assert!(matches!(result, Err(CallError::Internal(_))));
    assert!(notices.try_recv().is_err());
    let snapshot = crate::dao::ship_adaption::snapshot(&db, character.character).await.unwrap();
    assert!(snapshot.subject.is_none() && snapshot.chips.is_empty());
    assert_eq!(crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::Id(item)).await.unwrap()[0].quantity, 1);
    assert!(crate::dao::wallets::loyalty::balance(&db, character.character, fixture.issuer).await.unwrap().is_none());
    crate::services::tests::fixture::cleanup(&db, character).await.unwrap();
}
