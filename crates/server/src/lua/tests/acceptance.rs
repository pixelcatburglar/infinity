//! Real database acceptance coverage for Lua post-commit events and intercepts.
//! These tests use isolated game fixtures and the script-owned typed store.

use super::super::*;
use super::fixture;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::lua::events::GameEvent;
use crate::services::effects::online::OnlineChanged;
use crate::services::{self, CallError};
use std::time::Duration;

const SCRIPT: &str = "acceptance";
const DRIVER: &str = r#"
register.event('CharacterSelected', function(ev)
    if ev.user_id == -1 then
        store.set('cleanup_barrier', true)
        return
    end
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
#[ignore = "requires a dedicated initialized INFINITY_TEST_DATABASE_URL and real SDE; run this exact test in its own process"]
async fn real_database_events_and_intercepts() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap()
        .with_progress_registry(crate::progress::Registry::new(vec![crate::progress::lua_observer], vec![]).unwrap());
    assert!(crate::dao::lua_store::keys(&db, SCRIPT, "").await.unwrap().is_empty(), "acceptance script namespace must be empty");
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).expect("SDE station");
    let (station_id, system_id, region_id) = (station.station_id, station.solar_system_id.unwrap(), station.region_id.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_SHIP) && t.type_id != infinity_core::consts::TYPE_CAPSULE && t.group_id.is_some_and(|g| sde.repairable_ship.enters_recovery(t.type_id, g, infinity_core::consts::CATEGORY_SHIP) && !sde.repairable_ship.unrepairable(infinity_core::consts::CATEGORY_SHIP, g, t.type_id))).expect("SDE ship").type_id;
    let fixture = fixture::create(&db, ship_type, system_id).await.unwrap();
    db.execute("UPDATE game.characters SET home_station_id=$2 WHERE character_id=$1", &[&fixture.char_id, &station_id]).await.unwrap();
    let dir = std::env::temp_dir().join(format!("infinity-lua-acceptance-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("acceptance.lua"), DRIVER).unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let mut config = Config::default();
    config.lua.scripts_dir = dir.to_string_lossy().into_owned();
    config.lua.call_budget_ms = 15_000;
    config.lua.call_budget_instructions = 50_000_000;
    let hub = crate::chat::Hub::new(&config.chat.domain, db.clone(), world.clone());
    crate::lua::initialize_content(&config.lua, &db, &sde).await.unwrap();
    start(&config, &db, &sde, &world, &hub).await.unwrap();
    let test_db = db.clone();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        let world = test_world;
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
            |tx, outbox: &mut Outbox| Box::pin(async move { services::wallets::credit(tx, outbox, char_id, crate::isk::IskAmount::from_cents(2000).unwrap()).await }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(credited.is_ok());
        let failed: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move {
                services::wallets::credit(tx, outbox, char_id, crate::isk::IskAmount::from_cents(500).unwrap()).await?;
                Err(CallError::Internal(anyhow::anyhow!("intentional rollback")))
            }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(failed.is_err());
        barrier(&test_db, &world, 4, char_id).await;
        assert_eq!(get(&test_db, "wallet_count").await, Some(serde_json::json!(1)));
        assert_eq!(get(&test_db, "wallet_1").await, Some(serde_json::json!({"kind":"WalletChanged","owner_id":char_id,"account_key":infinity_core::consts::ACCOUNT_CASH,"balance":20.0,"delta":20.0,"observed":20.0})));
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), crate::isk::IskAmount::from_cents(2000));
        let cfg = Arc::new(config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let login = crate::handshake::Login { user_id: fixture.account, user_name: "acceptance".into(), role: GM_ROLES, client_id: 1, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(infinity_wire::conn::Conn::new(stream, address), cfg, test_db.clone(), sde.clone(), hub, world.clone(), login);
        client.change_session(vec![("charid", marshal::Value::Int(char_id)), ("stationid", marshal::Value::Int(station_id)), ("solarsystemid2", marshal::Value::Int(system_id))]);
        let mut type_id = None;
        let price = crate::isk::IskAmount::from_cents(1).unwrap();
        for candidate in sde.types.iter().filter(|t| t.type_id > 0 && t.market_group_id.is_some() && sde.is_tradable(t.type_id)) {
            if crate::dao::market::orders::asks(&test_db, crate::client::filetime_now(), infinity_core::consts::FILETIME_DAY, candidate.type_id, region_id, price).await.unwrap().is_empty() {
                type_id = Some(candidate.type_id);
                break;
            }
        }
        let type_id = type_id.expect("tradable SDE market type without matching asks");
        let funding: Result<(), CallError> = test_db.transaction(
            |tx, outbox: &mut Outbox| Box::pin(async move { services::wallets::credit(tx, outbox, char_id, crate::isk::IskAmount::from_cents(100000).unwrap()).await }),
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(funding.is_ok());
        crate::dao::lua_store::set(&test_db, SCRIPT, "allow_market", "true").await.unwrap();
        let args = [marshal::Value::Int(station_id), marshal::Value::Int(type_id.into()), marshal::Value::Float(0.01), marshal::Value::Int(1), marshal::Value::Int(0), marshal::Value::Int(1), marshal::Value::Int(1), marshal::Value::Bool(false), marshal::Value::None];
        let kwargs = marshal::Value::Dict(vec![]);
        let call = services::Call { service: "marketProxy", method: "PlaceBuyOrder", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
        assert!(services::dispatch(&mut client, &call).await.is_ok(), "allowed native market placement");
        assert_eq!(fixture::order_count(&test_db, char_id).await.unwrap(), 1);
        let orders_before = crate::dao::market::orders::my_orders(&test_db, char_id).await.unwrap();
        assert_eq!(orders_before.len(), 1);
        let balance = services::wallets::balance(&test_db, char_id).await.ok();
        let items = fixture::owned_item_count(&test_db, char_id).await.unwrap();
        crate::dao::lua_store::set(&test_db, SCRIPT, "allow_market", "false").await.unwrap();
        match services::dispatch(&mut client, &call).await {
            Err(CallError::User { msg, dict }) => { assert_eq!(msg, infinity_core::messages::MSG_TARGETING_ATTEMPT_CANCELLED); assert!(dict.is_empty()); }
            _ => panic!("market intercept did not refuse"),
        }
        assert_eq!(get(&test_db, "intercept_count").await, Some(serde_json::json!(2)));
        assert_eq!(fixture::order_count(&test_db, char_id).await.unwrap(), 1);
        let orders_after = crate::dao::market::orders::my_orders(&test_db, char_id).await.unwrap();
        assert_eq!(orders_after.len(), 1);
        assert_eq!((orders_after[0].id, orders_after[0].escrow, orders_after[0].vol_remaining, orders_after[0].price), (orders_before[0].id, orders_before[0].escrow, orders_before[0].vol_remaining, orders_before[0].price));
        assert_eq!(fixture::owned_item_count(&test_db, char_id).await.unwrap(), items);
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), balance);
        let claim = services::Call { service: "acceptance", method: "claimed", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        assert!(matches!(services::dispatch(&mut client, &claim).await, Ok(marshal::Value::Int(42))));
        assert_eq!(get(&test_db, "claim_saw_allow").await, Some(serde_json::json!(1)));
        world.system(system_id).await.unwrap();
        client.change_session(vec![
            ("stationid", marshal::Value::None),
            ("solarsystemid", marshal::Value::Int(system_id)),
            ("solarsystemid2", marshal::Value::Int(system_id)),
            ("shipid", marshal::Value::Int(fixture.ship_id)),
        ]);
        let snapshot = infinity_core::session::OnlineSession::from_session(&client.session).expect("client online snapshot");
        assert_eq!(world.online_session(char_id), Some(snapshot.clone()));
        assert_eq!((snapshot.ship_id, snapshot.system_id, snapshot.station_id), (Some(fixture.ship_id), Some(system_id), None));
        let identity = crate::services::identity::Identity { char_id, ship_id: Some(fixture.ship_id), corp_id: 1, station_id: None, system_id: Some(system_id) };
        let row = crate::dao::items::space_ships::FreeShip {
            item_id: fixture.ship_id, type_id: ship_type, owner_id: char_id,
            corporation_id: 1, alliance_id: -1, pos: [0.0; 3],
        };
        let prepared = services::ship::balls::piloted(&test_db, sde.clone(), &row, char_id, 0.0).await.ok().unwrap();
        world.with(system_id, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let ship_id = fixture.ship_id;
        let lethal = world.with(system_id, move |s| s.apply_damage(ship_id, [0.0; 4], crate::services::combat::damage::DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap()).await.unwrap();
        let input = crate::services::ship::loss::LossInput {
            position: lethal.position, source: lethal.source.clone(), lethal_at: lethal.lethal_at,
            contributions: lethal.contributions.clone(),
            victim: lethal.victim, context: lethal.victim_context, conflict: lethal.conflict,
            pending: std::sync::Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(None, [0.0; 3])),
        };
        let insured_at = crate::client::filetime_now();
        crate::dao::insurance::insure(&test_db, fixture.ship_id, char_id, ship_type, 1.0, insured_at, insured_at + 3600 * infinity_core::consts::FILETIME_SECOND).await.unwrap();
        let reload_id = crate::dao::items::pending_reloads::insert(&test_db, char_id, fixture.ship_id, fixture.ship_id, 0, ship_type, 1, infinity_core::consts::FLAG_CARGO, fixture.ship_id).await.unwrap();
        let owned_before_loss = fixture::owned_item_count(&test_db, char_id).await.unwrap();
        let balance_before_loss = services::wallets::balance(&test_db, char_id).await.ok();
        let wallet_events_before = crate::db::outbox::tests::wallet_events(&test_db, char_id).await.len();
        let notifications_before_loss = test_db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1", &[&char_id]).await.unwrap().len();
        // Exercise a real loss and force the enclosing transaction to abort before dispatch.
        let dispatched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rollback: Result<(), CallError> = test_db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
            let input = input.clone();
            let sde = sde.clone();
            Box::pin(async move {
                assert!(services::ship::loss::destroy_ship_in_transaction(tx, outbox, &identity, &sde, &input, None, &[]).await?.is_some());
                Err(CallError::Internal(anyhow::anyhow!("intentional rollback after ship loss")))
            })
        }, |(), _outbox| {
            let dispatched = dispatched.clone();
            async move {
                dispatched.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
        }).await;
        assert!(rollback.is_err());
        let (token, generation) = (lethal.token, lethal.generation);
        assert!(world.with(system_id, move |s| s.restore_known_rollback(ship_id, token, generation)).await.unwrap());
        assert!(!dispatched.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, char_id).await.unwrap(), Some(fixture.ship_id));
        assert_eq!(crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Id(fixture.ship_id)).await.unwrap().len(), 1);
        assert_eq!(test_db.query_one("SELECT count(*) AS n FROM game.killmails WHERE victim_item_id=$1", &[&fixture.ship_id]).await.unwrap().get::<_, i64>("n"), 0);
        let loss_key = crate::db::outbox::SourceKey::canonical("LossSettled", fixture.ship_id, 0);
        assert!(crate::dao::progress::source(&test_db, "business", loss_key.as_str()).await.unwrap().is_none());
        assert_eq!(fixture::owned_item_count(&test_db, char_id).await.unwrap(), owned_before_loss);
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), balance_before_loss);
        assert_eq!(test_db.query("SELECT notification_id FROM game.notifications WHERE receiver_id=$1", &[&char_id]).await.unwrap().len(), notifications_before_loss);
        assert_eq!(crate::dao::items::pending_reloads::for_ship(&test_db, fixture.ship_id).await.unwrap().iter().map(|r| r.reload_id).collect::<Vec<_>>(), vec![reload_id]);
        assert!(crate::dao::insurance::contract(&test_db, fixture.ship_id).await.unwrap().is_some());
        assert_eq!(get(&test_db, "ship_count").await, None);
        let lethal = world.with(system_id, move |s| s.apply_damage(ship_id, [0.0; 4], crate::services::combat::damage::DamageSource { force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default() }, true).unwrap().unwrap().first_lethal.unwrap()).await.unwrap();
        let input = crate::services::ship::loss::LossInput {
            position: lethal.position, source: lethal.source.clone(), lethal_at: lethal.lethal_at,
            contributions: lethal.contributions.clone(),
            victim: lethal.victim, context: lethal.victim_context, conflict: lethal.conflict,
            pending: std::sync::Arc::new(crate::services::ship::loss::facts::PlayerLossContext::new(None, [0.0; 3])),
        };
        let loss = {
            let _lock = services::market::lock().await;
            let (token, generation) = (lethal.token, lethal.generation);
            let expected = Some(snapshot);
            let loss = test_db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let input = input.clone();
                let sde = sde.clone();
                Box::pin(async move { services::ship::loss::destroy_ship_in_transaction(tx, outbox, &identity, &sde, &input, None, &[]).await })
            }, |value, outbox| { let world = world.clone(); async move { crate::services::effects::online::dispatch_changed_world(value, outbox, &world) } }).await.unwrap().unwrap();
            assert!(services::ship::loss::publish_loss(&identity, &test_db, &world, &sde, loss.clone(), token, generation, expected).await.ok().unwrap());
            loss
        };
        assert!(crate::dao::insurance::contract(&test_db, fixture.ship_id).await.unwrap().is_none());
        let balance_after_loss = services::wallets::balance(&test_db, char_id).await.ok();
        let receipt = crate::dao::progress::source(&test_db, "business", loss_key.as_str()).await.unwrap().expect("committed loss receipt");
        let receipt_id = receipt.event_id.clone();
        if let crate::lua::events::GameEvent::LossSettled { char_id: victim, loss_id, killmail_id, valuation, participants, .. } = receipt.event {
            assert_eq!((victim, loss_id), (char_id, fixture.ship_id));
            let actual: i64 = test_db.query_one("SELECT kill_id FROM game.killmails WHERE victim_item_id=$1", &[&fixture.ship_id]).await.unwrap().get("kill_id");
            assert_eq!(killmail_id, actual);
            assert!(participants.is_empty());
            assert_eq!(valuation.source, crate::lua::events::combat::ValuationSource::RetailPricesAverage);
            let wallet_events = crate::db::outbox::tests::wallet_events(&test_db, char_id).await;
            let payout = match &wallet_events[wallet_events_before..] {
                [] => crate::isk::IskAmount::ZERO,
                [crate::lua::events::GameEvent::WalletChanged { delta, .. }] => *delta,
                _ => panic!("unexpected insurance wallet events"),
            };
            assert_eq!(valuation.npc_insurance_paid, payout);
        } else { panic!("expected loss receipt"); }
        assert!(crate::dao::items::pending_reloads::for_ship(&test_db, fixture.ship_id).await.unwrap().is_empty());
        services::ship::lose_ship(&mut client, fixture.ship_id, loss.replacement_ship, loss.home, loss.position).await.unwrap();
        barrier(&test_db, &world, 5, char_id).await;
        let capsule = crate::dao::characters::selection::ship_id(&test_db, char_id).await.unwrap().expect("capsule");
        assert_ne!(capsule, fixture.ship_id);
        let rows = crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Id(fixture.ship_id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        let group: i64 = test_db.query_one("SELECT g.group_id FROM game.repairable_recovery_groups g JOIN game.repairable_recovery_items m ON m.group_id=g.group_id WHERE g.beneficiary_id=$1 AND m.item_id=$2", &[&char_id, &fixture.ship_id]).await.unwrap().get("group_id");
        assert_eq!((rows[0].type_id, rows[0].owner_id, rows[0].location_id, rows[0].flag_id, rows[0].quantity, rows[0].singleton), (ship_type, infinity_core::consts::NPC_CORP_INTERBUS, group, 0, 1, true));
        let rows = crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Id(capsule)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].type_id, infinity_core::consts::TYPE_CAPSULE);
        assert_eq!(client.session.int("shipid"), Some(capsule));
        assert_eq!(get(&test_db, "ship_1").await, Some(serde_json::json!({"kind":"ShipLost","char_id":char_id,"ship_id":fixture.ship_id,"type_id":ship_type,"system_id":system_id,"active":capsule})));
        let duplicate = {
            let _lock = services::market::lock().await;
            test_db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
                let input = input.clone();
                let sde = sde.clone();
                Box::pin(async move { services::ship::loss::destroy_ship_in_transaction(tx, outbox, &identity, &sde, &input, None, &[]).await })
            }, |value, outbox| { let world = world.clone(); async move { crate::services::effects::online::dispatch_changed_world(value, outbox, &world) } }).await.unwrap()
        };
        assert!(duplicate.is_none());
        assert_eq!(crate::dao::progress::source(&test_db, "business", loss_key.as_str()).await.unwrap().unwrap().event_id, receipt_id);
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), balance_after_loss);
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, char_id).await.unwrap(), Some(capsule));
        assert_eq!(test_db.query_one("SELECT count(*) AS n FROM game.killmails WHERE victim_item_id=$1", &[&fixture.ship_id]).await.unwrap().get::<_, i64>("n"), 1);
        barrier(&test_db, &world, 6, char_id).await;
        assert_eq!(get(&test_db, "ship_count").await, Some(serde_json::json!(1)));
        let items_before_wire = fixture::owned_item_count(&test_db, char_id).await.unwrap();
        let item_state = |rows: Vec<crate::dao::items::loading::GameItem>| {
            rows.into_iter().map(|item| (item.item_id, item.type_id, item.owner_id, item.location_id, item.flag_id, item.quantity, item.singleton, item.custom_info)).collect::<Vec<_>>()
        };
        let items_before_wire_state = item_state(crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Owner(char_id)).await.unwrap());
        let source = infinity_wire::macho::Address::Client { client_id: 1, call_id: marshal::Value::Int(42), service: None };
        let destination = infinity_wire::macho::Address::Any { service: Some("marketProxy".into()), call_id: marshal::Value::None };
        let call_payload = marshal::Value::tuple([
            marshal::Value::Int(1),
            marshal::Value::str("PlaceBuyOrder"),
            marshal::Value::Tuple(args.to_vec()),
            kwargs.clone(),
        ]);
        let request = infinity_wire::macho::Packet::new(
            infinity_wire::macho::cmd::CALL_REQ, source.clone(), destination.clone(), marshal::Value::Int(fixture.account),
            vec![marshal::Value::tuple([marshal::Value::Int(0), marshal::Value::str(marshal::dumps(&call_payload))])],
        );
        let mut registration = crate::live_sessions::register(fixture.account, false).expect("fixture session registration");
        registration.wait_for_predecessor().await;
        let run = client.run(registration);
        let peer_address = peer.peer_addr().unwrap();
        let mut peer = infinity_wire::conn::Conn::new(peer, peer_address);
        let response = tokio::time::timeout(Duration::from_secs(15), async {
            peer.send(&request.to_value()).await?;
            loop {
                let value = peer.recv().await?.ok_or_else(|| anyhow::anyhow!("client closed before call response"))?;
                let packet = infinity_wire::macho::Packet::from_value(&value)?;
                if packet.destination == source && (packet.command == infinity_wire::macho::cmd::ERRORRESPONSE || packet.command == infinity_wire::macho::cmd::CALL_RSP) {
                    break Ok::<_, anyhow::Error>(packet);
                }
            }
        }).await;
        use tokio::io::AsyncWriteExt;
        let shutdown = peer.stream.shutdown().await;
        drop(peer);
        // A failed exchange releases the session cooperatively and waits on the
        // account's completion barrier before panicking: fixture cleanup must
        // never race a session that is still alive.
        let response = match (response, shutdown) {
            (Ok(Ok(response)), Ok(())) => response,
            (response, shutdown) => {
                drop(run);
                let mut barrier = crate::live_sessions::register(fixture.account, true).expect("completion barrier registration");
                barrier.wait_for_predecessor().await;
                drop(barrier);
                shutdown.unwrap();
                panic!("timed out or failed waiting for correlated response: {response:?}");
            }
        };
        run.await.expect("client loop did not finish");
        assert_eq!(response.command, infinity_wire::macho::cmd::ERRORRESPONSE);
        assert_eq!(response.source, destination);
        assert_eq!(response.body.len(), 3);
        assert_eq!(response.body[0], marshal::Value::Int(infinity_wire::macho::cmd::CALL_REQ));
        assert_eq!(response.body[1], marshal::Value::Int(infinity_wire::macho::ERR_WRAPPEDEXCEPTION));
        let wrapped = response.body[2].items().expect("wrapped exception tuple");
        assert_eq!(wrapped.len(), 1);
        let exception = marshal::loads(wrapped[0].as_bytes().expect("marshalled exception")).unwrap();
        assert_eq!(exception, crate::client::user_error(infinity_core::messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
        assert_eq!(get(&test_db, "intercept_count").await, Some(serde_json::json!(3)));
        assert_eq!(fixture::order_count(&test_db, char_id).await.unwrap(), 1);
        let orders_wire = crate::dao::market::orders::my_orders(&test_db, char_id).await.unwrap();
        assert_eq!(orders_wire.len(), 1);
        assert_eq!((orders_wire[0].id, orders_wire[0].escrow, orders_wire[0].vol_remaining, orders_wire[0].price), (orders_before[0].id, orders_before[0].escrow, orders_before[0].vol_remaining, orders_before[0].price));
        assert_eq!(fixture::owned_item_count(&test_db, char_id).await.unwrap(), items_before_wire);
        assert_eq!(item_state(crate::dao::items::loading::load_items(&test_db, crate::dao::items::loading::ItemQuery::Owner(char_id)).await.unwrap()), items_before_wire_state);
        assert_eq!(services::wallets::balance(&test_db, char_id).await.ok(), balance_after_loss);
        let full_app = [21; 16];
        let closed_app = [22; 16];
        let mut full = world.notice_hub().register(full_app, 1);
        let mut closed = world.notice_hub().register(closed_app, 1);
        closed.rx.close();
        let state = crate::services::ship_cosmetics_mgr::state::State { ship_id: capsule, character_id: char_id, skin_id: None };
        let notice = crate::gateway::ship_cosmetics::notice(&state, &[char_id]).unwrap().remove(0);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let result: Result<(), CallError> = test_db.transaction(
            |_, outbox: &mut Outbox| {
                let first = notice.retarget(crate::gateway::protocol::target_group::Target::ApplicationInstanceUuid(full_app.to_vec())).unwrap();
                let second = notice.retarget(crate::gateway::protocol::target_group::Target::ApplicationInstanceUuid(full_app.to_vec())).unwrap();
                let third = notice.retarget(crate::gateway::protocol::target_group::Target::ApplicationInstanceUuid(closed_app.to_vec())).unwrap();
                let sender = sender.clone();
                Box::pin(async move {
                    outbox.notify_gateway(first);
                    outbox.notify_gateway(second);
                    outbox.notify_gateway(third);
                    outbox.notify_sender(sender, "acceptance", vec![marshal::Value::Int(char_id)]);
                    outbox.event(GameEvent::CharacterSelected { user_id: 7, char_id });
                    Ok(())
                })
            },
            |value, outbox| { let world = world.clone(); async move { dispatch_world(value, outbox, &world) } },
        ).await;
        assert!(result.is_ok());
        assert!(full.cancelled.try_recv().is_ok());
        assert!(closed.cancelled.try_recv().is_ok());
        assert!(full.rx.try_recv().is_ok());
        assert_eq!(receiver.try_recv().unwrap(), ("acceptance".to_string(), vec![marshal::Value::Int(char_id)]));
        await_count(&test_db, "selected_count", 7).await;
        assert_eq!(get(&test_db, "selected_7").await.unwrap()["user_id"], 7);
    }).await;
    let drained = crate::tests::catch_scenario(async {
        let drained: Result<(), CallError> = db.transaction(
            |_, outbox: &mut Outbox| Box::pin(async move {
                outbox.event(GameEvent::CharacterSelected { user_id: -1, char_id: fixture.char_id });
                Ok(())
            }),
            |value, outbox| async move { dispatch_world(value, outbox, &world) },
        ).await;
        assert!(drained.is_ok());
        tokio::time::timeout(Duration::from_secs(15), async {
            while get(&db, "cleanup_barrier").await != Some(serde_json::json!(true)) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("event runner did not reach cleanup barrier");
    }).await;
    let removed = std::fs::remove_dir_all(dir);
    // Recovery members are InterBus-owned; capture their keys and the owned
    // fixture rows before fixture::cleanup deletes them.
    let groups: Vec<i64> = db.query("SELECT group_id FROM game.repairable_recovery_groups WHERE beneficiary_id = $1", &[&fixture.char_id]).await.unwrap().iter().map(|row| row.get("group_id")).collect();
    let members: Vec<(i64, i64)> = db.query("SELECT m.member_id, m.item_id FROM game.repairable_recovery_items m JOIN game.repairable_recovery_groups g ON g.group_id = m.group_id WHERE g.beneficiary_id = $1 ORDER BY m.member_id", &[&fixture.char_id]).await.unwrap().iter().map(|row| (row.get("member_id"), row.get("item_id"))).collect();
    let jobs: Vec<String> = members.iter().flat_map(|&(member, _)| ["repairable.payment.expiry", "repairable.delivery.arrival", "repairable.withdrawal.expiry"].map(|kind| format!("{kind}:{member}"))).collect();
    let mut extra: Vec<i64> = members.iter().map(|&(_, item)| item).collect();
    extra.push(fixture.ship_id);
    if let Some(active) = crate::dao::characters::selection::ship_id(&db, fixture.char_id).await.unwrap() { extra.push(active); }
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: fixture.account, character: fixture.char_id }], &extra).await.unwrap();
    let cleaned = fixture::cleanup(&db, fixture, SCRIPT).await;
    removed.unwrap();
    cleaned.unwrap();
    if let Err(panic) = drained {
        std::panic::resume_unwind(panic);
    }
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_groups WHERE group_id = ANY($1)", &[&groups]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.repairable_recovery_items WHERE member_id = ANY($1)", &[&members.iter().map(|&(member, _)| member).collect::<Vec<_>>()]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.jobs WHERE job_id = ANY($1)", &[&jobs]).await.unwrap().is_empty());
    result.expect("database acceptance failed");
}
