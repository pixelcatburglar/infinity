//! Real database acceptance coverage for Lua post-commit events and intercepts.
//! These tests use isolated game fixtures and the script-owned typed store.

use super::super::*;
use super::fixture;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::lua::events::GameEvent;
use crate::services::{self, CallError};
use std::time::Duration;

const SCRIPT: &str = "acceptance";
const DRIVER: &str = r#"
register.event('CharacterSelected', function(ev)
    local n = (store.get('selected_count') or 0) + 1
    store.set('selected_' .. n, {kind=ev.kind, user_id=ev.user_id, char_id=ev.char_id, marker=store.get('marker')})
    store.set('selected_count', n)
end)
register.event('WalletChanged', function(ev)
    local n = (store.get('wallet_count') or 0) + 1
    store.set('wallet_' .. n, {kind=ev.kind, owner_id=ev.owner_id, account_key=ev.account_key, balance=ev.balance, delta=ev.delta, observed=dao.wallets.balance(ev.owner_id)})
    store.set('wallet_count', n)
end)
register.event('ShipLost', function(ev)
    local n = (store.get('ship_count') or 0) + 1
    store.set('ship_' .. n, {kind=ev.kind, char_id=ev.char_id, ship_id=ev.ship_id, type_id=ev.type_id, system_id=ev.system_id, active=dao.characters.ship(ev.char_id)})
    store.set('ship_count', n)
end)
register.intercept('marketProxy', 'PlaceBuyOrder', function(ctx, args, kwargs)
    store.set('intercept_count', (store.get('intercept_count') or 0) + 1)
    if store.get('allow_market') then return true end
    return false, 'TargetingAttemptCancelled'
end)
register.intercept('acceptance', 'claimed', function(ctx, args, kwargs)
    store.set('allow_count', (store.get('allow_count') or 0) + 1)
    return true
end)
register.rpc('acceptance', 'claimed', function(ctx, args, kwargs)
    store.set('claim_saw_allow', store.get('allow_count'))
    return eve.int(42)
end)
"#;

async fn get(db: &Db, key: &str) -> Option<serde_json::Value> {
    crate::dao::lua_store::get(db, SCRIPT, key).await.unwrap().map(|s| serde_json::from_str(&s).unwrap())
}

async fn await_count(db: &Db, key: &str, count: i64) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if get(db, key).await == Some(serde_json::json!(count)) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("event runner did not reach barrier");
}

async fn barrier(db: &Db, world: &Arc<World>, n: i64, character: i64) {
    let result: Result<(), CallError> = db.transaction(
        |_, outbox: &mut Outbox| Box::pin(async move {
            outbox.event(GameEvent::CharacterSelected { user_id: n, char_id: character });
            Ok(())
        }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await;
    assert!(result.is_ok());
    await_count(db, "selected_count", n).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn real_database_events_and_intercepts() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(crate::dao::lua_store::keys(&db, SCRIPT, "").await.unwrap().is_empty(), "acceptance script namespace must be empty");
    let before = fixture::fingerprint(&db).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).expect("SDE station");
    let (station_id, system_id, region_id) = (station.station_id, station.solar_system_id.unwrap(), station.region_id.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP) && t.type_id != crate::consts::TYPE_CAPSULE).expect("SDE ship").type_id;
    let fixture = fixture::create(&db, ship_type, system_id).await.unwrap();
    let dir = std::env::temp_dir().join(format!("infinity-lua-acceptance-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("acceptance.lua"), DRIVER).unwrap();
    let task_dir = dir.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let sde = sde.clone();
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let mut config = Config::default();
        config.lua.scripts_dir = task_dir.to_string_lossy().into_owned();
        config.lua.call_budget_ms = 15_000;
        config.lua.call_budget_instructions = 50_000_000;
        let hub = crate::chat::Hub::new(&config.chat.domain, test_db.clone(), world.clone());
        start(&config, &test_db, &sde, &world, &hub).await.unwrap();
        let char_id = fixture.char_id;
        let result: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move {
                crate::dao::lua_store::set(tx, SCRIPT, "marker", "17").await?;
                outbox.event(GameEvent::CharacterSelected { user_id: 11, char_id });
                outbox.event(GameEvent::CharacterSelected { user_id: 12, char_id });
                Ok(())
            }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(result.is_ok());
        await_count(&test_db, "selected_count", 2).await;
        assert_eq!(get(&test_db, "selected_1").await, Some(serde_json::json!({"kind":"CharacterSelected","user_id":11,"char_id":char_id,"marker":17})));
        assert_eq!(get(&test_db, "selected_2").await, Some(serde_json::json!({"kind":"CharacterSelected","user_id":12,"char_id":char_id,"marker":17})));
        let failed: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move {
                crate::dao::lua_store::set(tx, SCRIPT, "marker", "99").await?;
                outbox.event(GameEvent::CharacterSelected { user_id: 99, char_id });
                Err(CallError::Internal(anyhow::anyhow!("intentional rollback")))
            }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(failed.is_err());
        barrier(&test_db, &world, 3, char_id).await;
        assert_eq!(get(&test_db, "marker").await, Some(serde_json::json!(17)));
        assert_eq!(get(&test_db, "selected_3").await.unwrap()["user_id"], 3);
        assert_eq!(get(&test_db, "selected_count").await, Some(serde_json::json!(3)));
        let credited: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move { services::wallets::credit(tx, outbox, char_id, 20.0).await }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(credited.is_ok());
        let failed: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move {
                services::wallets::credit(tx, outbox, char_id, 5.0).await?;
                Err(CallError::Internal(anyhow::anyhow!("intentional rollback")))
            }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(failed.is_err());
        barrier(&test_db, &world, 4, char_id).await;
        assert_eq!(get(&test_db, "wallet_count").await, Some(serde_json::json!(1)));
        assert_eq!(get(&test_db, "wallet_1").await, Some(serde_json::json!({"kind":"WalletChanged","owner_id":char_id,"account_key":crate::consts::ACCOUNT_CASH,"balance":20.0,"delta":20.0,"observed":20.0})));
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), Some(20.0));
        let cfg = Arc::new(config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "acceptance".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, test_db.clone(), sde.clone(), hub, world.clone(), login);
        client.change_session(vec![("charid", marshal::Value::Int(char_id))]);
        let mut type_id = None;
        for candidate in sde.types.iter().filter(|t| t.type_id > 0) {
            if crate::dao::market::orders::asks(&test_db, 0, candidate.type_id, region_id, 0.01).await.unwrap().is_empty() {
                type_id = Some(candidate.type_id);
                break;
            }
        }
        let type_id = type_id.expect("SDE type without matching asks");
        let funding: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move { services::wallets::credit(tx, outbox, char_id, 1000.0).await }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(funding.is_ok());
        crate::dao::lua_store::set(&test_db, SCRIPT, "allow_market", "true").await.unwrap();
        let args = [marshal::Value::Int(station_id), marshal::Value::Int(type_id.into()), marshal::Value::Float(0.01), marshal::Value::Int(1), marshal::Value::Int(0), marshal::Value::Int(1), marshal::Value::Int(1), marshal::Value::Bool(false), marshal::Value::Float(0.0)];
        let kwargs = marshal::Value::Dict(vec![]);
        let call = services::Call { service: "marketProxy", method: "PlaceBuyOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(services::dispatch(&mut client, &call).await.is_ok(), "allowed native market placement");
        assert_eq!(fixture::order_count(&test_db, char_id).await.unwrap(), 1);
        let orders_before = crate::dao::market::orders::my_orders(&test_db, char_id, 0).await.unwrap();
        assert_eq!(orders_before.len(), 1);
        let balance = services::wallets::balance(&test_db, char_id).await.ok();
        let items = fixture::owned_item_count(&test_db, char_id).await.unwrap();
        crate::dao::lua_store::set(&test_db, SCRIPT, "allow_market", "false").await.unwrap();
        match services::dispatch(&mut client, &call).await {
            Err(CallError::User { msg, dict }) => { assert_eq!(msg, crate::messages::MSG_TARGETING_ATTEMPT_CANCELLED); assert!(dict.is_empty()); }
            _ => panic!("market intercept did not refuse"),
        }
        assert_eq!(get(&test_db, "intercept_count").await, Some(serde_json::json!(2)));
        assert_eq!(fixture::order_count(&test_db, char_id).await.unwrap(), 1);
        let orders_after = crate::dao::market::orders::my_orders(&test_db, char_id, 0).await.unwrap();
        assert_eq!(orders_after.len(), 1);
        assert_eq!((orders_after[0].id, orders_after[0].escrow, orders_after[0].vol_remaining, orders_after[0].price), (orders_before[0].id, orders_before[0].escrow, orders_before[0].vol_remaining, orders_before[0].price));
        assert_eq!(fixture::owned_item_count(&test_db, char_id).await.unwrap(), items);
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), balance);
        let claim = services::Call { service: "acceptance", method: "claimed", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(matches!(services::dispatch(&mut client, &claim).await, Ok(marshal::Value::Int(42))));
        assert_eq!(get(&test_db, "claim_saw_allow").await, Some(serde_json::json!(1)));
        world.system(system_id).await.unwrap();
        client.change_session(vec![
            ("solarsystemid", marshal::Value::Int(system_id)),
            ("shipid", marshal::Value::Int(fixture.ship_id)),
        ]);
        services::ship_destroyed(&mut client, &[marshal::Value::Int(fixture.ship_id)]).await;
        barrier(&test_db, &world, 5, char_id).await;
        let capsule = crate::dao::characters::selection::ship_id(&test_db, char_id).await.unwrap().expect("capsule");
        assert_ne!(capsule, fixture.ship_id);
        let rows = crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Id(fixture.ship_id)).await.unwrap();
        assert!(rows.is_empty());
        let rows = crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Id(capsule)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].type_id, crate::consts::TYPE_CAPSULE);
        assert_eq!(client.session.int("shipid"), Some(capsule));
        assert_eq!(get(&test_db, "ship_1").await, Some(serde_json::json!({"kind":"ShipLost","char_id":char_id,"ship_id":fixture.ship_id,"type_id":ship_type,"system_id":system_id,"active":capsule})));
        services::ship_destroyed(&mut client, &[marshal::Value::Int(fixture.ship_id)]).await;
        barrier(&test_db, &world, 6, char_id).await;
        assert_eq!(get(&test_db, "ship_count").await, Some(serde_json::json!(1)));
        let items_before_wire = fixture::owned_item_count(&test_db, char_id).await.unwrap();
        let item_state = |rows: Vec<crate::dao::items::loading::GameItem>| {
            rows.into_iter().map(|item| (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton, item.custom_info)).collect::<Vec<_>>()
        };
        let items_before_wire_state = item_state(crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Owner(char_id)).await.unwrap());
        let source = crate::macho::Address::Client { client_id: 1, call_id: marshal::Value::Int(42), service: None };
        let destination = crate::macho::Address::Any { service: Some("marketProxy".into()), call_id: marshal::Value::None };
        let call_payload = marshal::Value::tuple([
            marshal::Value::Int(1),
            marshal::Value::str("PlaceBuyOrder"),
            marshal::Value::Tuple(args.to_vec()),
            kwargs.clone(),
        ]);
        let request = crate::macho::Packet::new(
            crate::macho::cmd::CALL_REQ, source.clone(), destination.clone(), marshal::Value::Int(fixture.account),
            vec![marshal::Value::tuple([marshal::Value::Int(0), marshal::Value::str(marshal::dumps(&call_payload))])],
        );
        let mut registration = crate::live_sessions::register(fixture.account, false).expect("fixture session registration");
        registration.wait_for_predecessor().await;
        let mut client_task = tokio::spawn(async move { client.run(&mut registration).await });
        let peer_address = peer.peer_addr().unwrap();
        let mut peer = crate::conn::Conn::new(peer, peer_address);
        let response = tokio::time::timeout(Duration::from_secs(15), async {
            peer.send(&request.to_value()).await?;
            loop {
                let value = peer.recv().await?.ok_or_else(|| anyhow::anyhow!("client closed before call response"))?;
                let packet = crate::macho::Packet::from_value(&value)?;
                if packet.destination == source && (packet.command == crate::macho::cmd::ERRORRESPONSE || packet.command == crate::macho::cmd::CALL_RSP) {
                    break Ok::<_, anyhow::Error>(packet);
                }
            }
        }).await;
        use tokio::io::AsyncWriteExt;
        let shutdown = peer.stream.shutdown().await;
        let client_result = tokio::time::timeout(Duration::from_secs(15), &mut client_task).await;
        if client_result.is_err() {
            client_task.abort();
            let _ = client_task.await;
        }
        drop(peer);
        shutdown.unwrap();
        client_result.expect("client loop did not finish").unwrap().unwrap();
        let response = response.expect("timed out waiting for correlated response").unwrap();
        assert_eq!(response.command, crate::macho::cmd::ERRORRESPONSE);
        assert_eq!(response.source, destination);
        assert_eq!(response.body.len(), 3);
        assert_eq!(response.body[0], marshal::Value::Int(crate::macho::cmd::CALL_REQ));
        assert_eq!(response.body[1], marshal::Value::Int(crate::macho::ERR_WRAPPEDEXCEPTION));
        let wrapped = response.body[2].items().expect("wrapped exception tuple");
        assert_eq!(wrapped.len(), 1);
        let exception = marshal::loads(wrapped[0].as_bytes().expect("marshalled exception")).unwrap();
        assert_eq!(exception, crate::client::user_error(crate::messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
        assert_eq!(get(&test_db, "intercept_count").await, Some(serde_json::json!(3)));
        assert_eq!(fixture::order_count(&test_db, char_id).await.unwrap(), 1);
        let orders_wire = crate::dao::market::orders::my_orders(&test_db, char_id, 0).await.unwrap();
        assert_eq!(orders_wire.len(), 1);
        assert_eq!((orders_wire[0].id, orders_wire[0].escrow, orders_wire[0].vol_remaining, orders_wire[0].price), (orders_before[0].id, orders_before[0].escrow, orders_before[0].vol_remaining, orders_before[0].price));
        assert_eq!(fixture::owned_item_count(&test_db, char_id).await.unwrap(), items_before_wire);
        assert_eq!(item_state(crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Owner(char_id)).await.unwrap()), items_before_wire_state);
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), balance);
    }).await;
    std::fs::remove_dir_all(dir).unwrap();
    fixture::cleanup(&db, fixture, SCRIPT).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    result.expect("database acceptance failed");
}
