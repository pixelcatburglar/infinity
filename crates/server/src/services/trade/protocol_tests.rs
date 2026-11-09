//! Real database service-path regressions; run serially against the isolated test database.
use std::sync::Arc;
use marshal::Value;
use crate::{client::Client, config::{Config, deployment::DatabaseConfig}, conn::Conn, dao::{items::transfers, trades}, db::Db, handshake::Login, sde::Sde, services::{Call, CallResult}, space::World};

async fn client(db: Db, sde: Arc<Sde>, world: Arc<World>, user: i64, character: i64, station: i64) -> (Client, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let login = Login { user_id: user, user_name: user.to_string(), role: 0, client_id: character, session_id: character, language_id: "EN".into() };
    let chat = crate::chat::Hub::new("localhost", db.clone(), world.clone());
    let mut client = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), db, sde, chat, world, login);
    client.session.apply(vec![("userid", Value::Int(user)), ("charid", Value::Int(character)), ("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
    (client, peer)
}

async fn call(c: &mut Client, method: &str, args: &[Value], id: Option<i64>, kwargs: &Value) -> CallResult {
    let bind = id.map(Value::Int);
    super::super::route(c, &Call { service: "trademgr", method, args, kwargs, bound: id.is_some(), bind_params: bind.as_ref() }).await
}

async fn assert_timeout(db: &Db, id: i64) -> i64 {
    let trade = trades::get(db, id).await.unwrap().unwrap();
    let row = db.query_one("SELECT due_at, payload FROM game.jobs WHERE job_id = $1", &[&format!("trade-timeout:{id}")]).await.unwrap();
    let due: i64 = row.get("due_at");
    assert_eq!(due, trade.updated_at + crate::client::filetime_ticks_from_seconds(crate::config::game().trade.idle_timeout_secs));
    assert_eq!(row.get::<_, String>("payload"), id.to_string());
    due
}

async fn assert_no_timeout(db: &Db, id: i64) {
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&format!("trade-timeout:{id}")]).await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn staging_rejects_restricted_and_foreign_descendants_atomically() {
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let station = sde.map.stations().next().expect("station required").station_id;
    let normal = sde.types.iter().find(|t| sde.is_tradable(t.type_id)).expect("tradable type required").type_id;
    let lists = crate::dao::sde::typelists::load(&db, &["36".to_string(), "142".to_string()]).await.unwrap();
    assert_eq!(lists.len(), 2);
    let mut restricted = Vec::new();
    for list in lists {
        let ty = sde.types.iter().find(|t| sde.typelists.contains(list.id, t.type_id, t.group_id, t.category_id))
            .expect("each restriction list must include an SDE type").type_id;
        restricted.push(ty);
    }
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let a: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let b: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let (mut c, _socket) = client(db.clone(), sde.clone(), world.clone(), account, a, station).await;
    let (ta, mut ra) = tokio::sync::mpsc::unbounded_channel();
    let (tb, mut rb) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(a, ta); world.register_pilot(b, tb);
    let empty = Value::Dict(vec![]);
    assert!(call(&mut c, "InitiateTrade", &[Value::Int(b)], None, &empty).await.is_ok());
    let trade = trades::open_for(&db, a).await.unwrap().unwrap();
    while ra.try_recv().is_ok() {} while rb.try_recv().is_ok() {}
    let good = transfers::insert(&db, normal, a, station, infinity_core::consts::FLAG_HANGAR, 3).await.unwrap();
    let root = transfers::insert(&db, normal, a, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let leaf = transfers::insert(&db, restricted[0], a, root, 5, 1).await.unwrap();
    let args = [Value::list([Value::Int(good), Value::Int(root)]), Value::Int(station)];
    c.session.apply(vec![("shipid", Value::Int(root))]);
    assert!(matches!(call(&mut c, "MultiAdd", &args, Some(trade.id), &empty).await, Err(crate::services::CallError::User { msg, .. }) if msg == infinity_core::messages::trade::MSG_PEOPLE_ABOARD_SHIP));
    db.execute("UPDATE game.items SET flag_id = $2 WHERE item_id = $1", &[&root, &infinity_core::consts::FLAG_CARGO]).await.unwrap();
    assert!(matches!(call(&mut c, "MultiAdd", &args, Some(trade.id), &empty).await, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::trade::TRADE_SOURCE_NOT_HANGAR))]));
    db.execute("UPDATE game.items SET flag_id = $2 WHERE item_id = $1", &[&root, &infinity_core::consts::FLAG_HANGAR]).await.unwrap();
    c.session.apply(vec![("shipid", Value::Int(0))]);
    let cargo_source = [Value::list([Value::Int(root)]), Value::Int(root)];
    assert!(matches!(call(&mut c, "MultiAdd", &cargo_source, Some(trade.id), &empty).await, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::trade::TRADE_SOURCE_NOT_HANGAR))]));
    assert!(trades::items(&db, trade.id).await.unwrap().is_empty());
    assert!(ra.try_recv().is_err() && rb.try_recv().is_err());
    for type_id in restricted {
        db.execute("UPDATE game.items SET type_id = $2 WHERE item_id = $1", &[&leaf, &type_id]).await.unwrap();
        assert!(call(&mut c, "MultiAdd", &args, Some(trade.id), &empty).await.is_err());
        assert!(trades::items(&db, trade.id).await.unwrap().is_empty());
    }
    let rows = db.query("SELECT item_id, owner_id, location_id, quantity FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![good, root, leaf]]).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get::<_, i64>("location_id"), station);
    assert_eq!(rows[0].get::<_, i64>("quantity"), 3);
    assert_eq!(rows[1].get::<_, i64>("location_id"), station);
    assert_eq!(rows[2].get::<_, i64>("location_id"), root);
    assert!(ra.try_recv().is_err() && rb.try_recv().is_err());
    db.execute("UPDATE game.items SET type_id = $2, owner_id = $3 WHERE item_id = $1", &[&leaf, &normal, &b]).await.unwrap();
    assert!(call(&mut c, "MultiAdd", &args, Some(trade.id), &empty).await.is_err());
    assert!(trades::items(&db, trade.id).await.unwrap().is_empty());
    assert!(ra.try_recv().is_err() && rb.try_recv().is_err());
    assert!(call(&mut c, "Abort", &[], Some(trade.id), &empty).await.is_ok());
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![good, root, leaf]]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&trade.id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&vec![a, b]]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn clone_dispatch_refusals_preserve_trade_and_success_cancels() {
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let stations: Vec<_> = sde.map.stations().filter(|s| s.solar_system_id.is_some() && s.constellation_id.is_some() && s.region_id.is_some()).take(2).map(|s| s.station_id).collect();
    assert_eq!(stations.len(), 2);
    let (station, dest) = (stations[0], stations[1]);
    let world = World::new(db.clone(), sde.clone(), 1);
    let normal = sde.types.iter().find(|t| sde.is_tradable(t.type_id)).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let fee = crate::config::game().clones.service_fee_isk;
    let a: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, $3::float8::numeric + 100) RETURNING character_id", &[&account, &station, &fee]).await.unwrap().get("character_id");
    let b: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let capsule = crate::dao::items::creation::rookie_ship(&db, infinity_core::consts::TYPE_CAPSULE, a, station, infinity_core::consts::FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&a, &capsule]).await.unwrap();
    let (mut c, _socket) = client(db.clone(), sde.clone(), world.clone(), account, a, station).await;
    c.session.apply(vec![("shipid", Value::Int(capsule))]);
    let (ta, mut ra) = tokio::sync::mpsc::unbounded_channel();
    let (tb, mut rb) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(a, ta); world.register_pilot(b, tb);
    let empty = Value::Dict(vec![]);
    assert!(call(&mut c, "InitiateTrade", &[Value::Int(b)], None, &empty).await.is_ok());
    let trade = trades::open_for(&db, a).await.unwrap().unwrap();
    let root = transfers::insert(&db, normal, a, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    assert!(call(&mut c, "Add", &[Value::Int(root), Value::Int(station)], Some(trade.id), &empty).await.is_ok());
    let dest_clone: i64 = db.query_one("INSERT INTO game.jump_clones (owner_id, location_id) VALUES ($1, $2) RETURNING clone_id", &[&a, &dest]).await.unwrap().get("clone_id");
    let same_clone: i64 = db.query_one("INSERT INTO game.jump_clones (owner_id, location_id) VALUES ($1, $2) RETURNING clone_id", &[&a, &station]).await.unwrap().get("clone_id");
    while ra.try_recv().is_ok() {} while rb.try_recv().is_ok() {}
    db.execute("UPDATE game.characters SET last_clone_jump = $2 WHERE character_id = $1", &[&a, &crate::client::filetime_now()]).await.unwrap();
    for (location, clone, confirmed, cooldown) in [(dest, dest_clone, true, true), (dest, i64::MAX, true, false), (dest, same_clone, true, false), (dest, dest_clone, false, false)] {
        if !cooldown { db.execute("UPDATE game.characters SET last_clone_jump = NULL WHERE character_id = $1", &[&a]).await.unwrap(); }
        let args = [Value::Int(location), Value::Int(clone), Value::Float(fee), Value::Bool(confirmed)];
        let call = Call { service: "jumpCloneSvc", method: "CloneJump", args: &args, kwargs: &empty, bound: false, bind_params: None };
        assert!(crate::services::clones::dispatch(&mut c, &call).await.unwrap().is_err());
        assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "open");
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id = $1", &[&root]).await.unwrap().get::<_, i64>("location_id"), trade.id);
        assert!(ra.try_recv().is_err() && rb.try_recv().is_err());
    }
    let args = [Value::Int(dest), Value::Int(dest_clone), Value::Float(fee), Value::Bool(true)];
    let call = Call { service: "jumpCloneSvc", method: "CloneJump", args: &args, kwargs: &empty, bound: false, bind_params: None };
    crate::services::clones::dispatch(&mut c, &call).await.unwrap().expect("funded valid clone jump must cancel trade");
    assert_eq!(crate::dao::wallets::characters::balance(&db, a).await.unwrap(), crate::isk::IskAmount::from_cents(10_000).unwrap());
    assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "cancelled");
    assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id = $1", &[&root]).await.unwrap().get::<_, i64>("location_id"), station);
    let mut events_a = Vec::new(); while let Ok((event, _)) = ra.try_recv() { events_a.push(event); }
    let mut events_b = Vec::new(); while let Ok((event, _)) = rb.try_recv() { events_b.push(event); }
    assert!(events_a.iter().any(|event| event == "OnTradeCancel"));
    assert!(events_b.iter().any(|event| event == "OnTradeCancel"));
    db.execute("DELETE FROM game.items WHERE owner_id = $1", &[&a]).await.unwrap();
    db.execute("DELETE FROM game.jump_clones WHERE owner_id = $1", &[&a]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&trade.id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&vec![a, b]]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn two_clients_settle_through_service_routes() {
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| sde.map.celestial(s.station_id).is_some_and(|c| c.type_id.is_some())).expect("station celestial required").station_id;
    let normal = sde.types.iter().find(|t| sde.is_tradable(t.type_id)).unwrap().type_id;
    let world = World::new(db.clone(), sde.clone(), 1);
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let a: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let b: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let third: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let (mut ca, _socket_a) = client(db.clone(), sde.clone(), world.clone(), account, a, station).await;
    let (mut cb, _socket_b) = client(db.clone(), sde.clone(), world.clone(), account, b, station).await;
    let (ta, mut ra) = tokio::sync::mpsc::unbounded_channel();
    let (tb, initial_rb) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(a, ta); world.register_pilot(b, tb);
    world.set_online_session(a, infinity_core::session::OnlineSession::from_session(&ca.session));
    let empty = Value::Dict(vec![]);
    macro_rules! refusal {
        ($result:expr, $message:expr) => {
            assert!(matches!($result, Err(crate::services::CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode($message))]));
        };
    }
    ca.session.apply(vec![("stationid", Value::None)]);
    refusal!(call(&mut ca, "InitiateTrade", &[Value::Int(a)], None, &empty).await, infinity_core::messages::trade::TRADE_SELF);
    refusal!(call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await, infinity_core::messages::trade::TRADE_NOT_DOCKED);
    ca.session.apply(vec![("stationid", Value::Int(station))]);
    world.unregister_pilot_generation(b, None);
    drop(initial_rb);
    refusal!(call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await, infinity_core::messages::trade::TRADE_TARGET_NOT_HERE);
    let (tb, mut rb) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(b, tb);
    let other_station = sde.map.stations().find(|s| s.station_id != station).expect("second station required").station_id;
    db.execute("UPDATE game.characters SET station_id = $2 WHERE character_id = $1", &[&b, &other_station]).await.unwrap();
    refusal!(call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await, infinity_core::messages::trade::TRADE_TARGET_NOT_HERE);
    db.execute("UPDATE game.characters SET station_id = $2 WHERE character_id = $1", &[&b, &station]).await.unwrap();
    let busy = trades::create(&db, station, b, third, crate::client::filetime_now()).await.unwrap();
    refusal!(call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await, infinity_core::messages::trade::TRADE_BUSY);
    db.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&busy.id]).await.unwrap();
    let moniker = call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await.ok().expect("initiation");
    let trade = trades::open_for(&db, a).await.unwrap().unwrap();
    let initial_due = assert_timeout(&db, trade.id).await;
    assert_eq!(moniker, Value::Instance(Box::new(Value::str("util.Moniker")), Box::new(Value::tuple([Value::str("trademgr"), Value::Int(ca.cfg.server.node_id), Value::Int(trade.id), Value::None]))));
    assert_eq!(call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await.ok().expect("same-pair initiation"), moniker);
    assert_eq!(trades::open_for(&db, b).await.unwrap().unwrap().id, trade.id);
    let count = db.query_one("SELECT count(*) AS trade_count FROM game.trades WHERE (char_a = $1 AND char_b = $2) OR (char_a = $2 AND char_b = $1)", &[&a, &b]).await.unwrap();
    assert_eq!(count.get::<_, i64>("trade_count"), 1);
    let initiated = rb.try_recv().expect("target initiation notification");
    assert_eq!(initiated, ("OnTradeInitiate".into(), vec![Value::Int(a), moniker]));
    assert!(rb.try_recv().is_err());
    let bind_args = [Value::Int(trade.id), Value::tuple([Value::str("List"), Value::tuple([]), empty.clone()])];
    let bind_call = Call { service: "trademgr", method: "MachoBindObject", args: &bind_args, kwargs: &empty, bound: false, bind_params: None };
    let bound = crate::services::dispatch(&mut cb, &bind_call).await.ok().expect("target bind");
    let [callback, list] = bound.items().expect("bind tuple") else { panic!("bind result must have two elements") };
    assert!(matches!(callback, Value::Callback(_)));
    assert_eq!(crate::services::value_field(list, "tradeContainerID").and_then(Value::as_int), Some(trade.id));
    assert!(call(&mut cb, "GetItem", &[], Some(trade.id), &empty).await.is_ok());
    assert_eq!(assert_timeout(&db, trade.id).await, initial_due);
    let stack = transfers::insert(&db, normal, a, station, infinity_core::consts::FLAG_HANGAR, 4_294_967_299).await.unwrap();
    let root = transfers::insert(&db, normal, a, station, infinity_core::consts::FLAG_HANGAR, 3).await.unwrap();
    let child = transfers::insert(&db, normal, a, root, 5, 3).await.unwrap();
    let qty = Value::dict([("qty", Value::Int(4_294_967_296))]);
    assert!(call(&mut ca, "Add", &[Value::Int(stack), Value::Int(station)], Some(trade.id), &qty).await.is_ok());
    let staged_quantity: i64 = db.query_one("SELECT quantity FROM game.items WHERE location_id = $1 AND owner_id = $2", &[&trade.id, &a]).await.unwrap().get("quantity");
    assert_eq!(staged_quantity, 4_294_967_296);
    assert_timeout(&db, trade.id).await;
    assert!(call(&mut ca, "MultiAdd", &[Value::list([Value::Int(root)]), Value::Int(station)], Some(trade.id), &empty).await.is_ok());
    assert_timeout(&db, trade.id).await;
    let staged = trades::items(&db, trade.id).await.unwrap();
    assert_eq!(staged.len(), 2);
    let bind = Value::tuple([Value::Int(station), Value::Int(10004), Value::Int(a)]);
    let foreign = transfers::insert(&db, normal, b, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    let foreign_args = [Value::Int(foreign), Value::Int(station)];
    let foreign_add = Call { service: "invInventory", method: "Add", args: &foreign_args, kwargs: &empty, bound: true, bind_params: Some(&bind) };
    assert!(crate::services::inventory::dispatch(&mut ca, &foreign_add).await.expect("inventory route").is_err());
    let foreign_row = db.query_one("SELECT owner_id, location_id FROM game.items WHERE item_id = $1", &[&foreign]).await.unwrap();
    assert_eq!(foreign_row.get::<_, i64>("owner_id"), b);
    assert_eq!(foreign_row.get::<_, i64>("location_id"), station);
    let plex_args = [Value::Int(1), Value::Int(1), Value::str("VaultToDirectTrade")];
    let plex = Call { service: "invbroker", method: "WithdrawPlexFromVault", args: &plex_args, kwargs: &empty, bound: false, bind_params: None };
    refusal!(crate::services::inventory::dispatch(&mut ca, &plex).await.expect("PLEX route"), infinity_core::messages::trade::TRADE_PLEX_UNSUPPORTED);
    for id in [root, child] {
        let args = [Value::Int(id), Value::Int(trade.id)];
        let icall = Call { service: "invInventory", method: "Add", args: &args, kwargs: &empty, bound: true, bind_params: Some(&bind) };
        assert!(crate::services::inventory::dispatch(&mut ca, &icall).await.expect("inventory route").is_err());
        let args = [Value::list([Value::Int(id)])];
        let bcall = Call { service: "invbroker", method: "TrashItems", args: &args, kwargs: &empty, bound: true, bind_params: None };
        assert!(crate::services::inventory::dispatch(&mut ca, &bcall).await.unwrap().is_err());
        let args = [Value::Int(id)];
        let bcall = Call { service: "invbroker", method: "GetInventoryFromId", args: &args, kwargs: &empty, bound: true, bind_params: None };
        assert!(crate::services::inventory::dispatch(&mut ca, &bcall).await.unwrap().is_err());
    }
    for id in [root, child] {
        let args = [Value::Int(root), Value::Int(id), Value::Int(1), Value::Int(a)];
        let split = Call { service: "invbroker", method: "SplitStack", args: &args, kwargs: &empty, bound: true, bind_params: None };
        assert!(crate::services::inventory::dispatch(&mut ca, &split).await.unwrap().is_err());
        let args = [Value::list([Value::tuple([Value::Int(id), Value::Int(stack), Value::Int(1)])])];
        let merge = Call { service: "invInventory", method: "MultiMerge", args: &args, kwargs: &empty, bound: true, bind_params: Some(&bind) };
        assert!(crate::services::inventory::dispatch(&mut ca, &merge).await.expect("inventory route").is_err());
        let item_bind = Value::tuple([Value::Int(id)]);
        let stack_all = Call { service: "invInventory", method: "StackAll", args: &[], kwargs: &empty, bound: true, bind_params: Some(&item_bind) };
        assert!(crate::services::inventory::dispatch(&mut ca, &stack_all).await.expect("inventory route").is_err());
    }
    assert!(call(&mut ca, "OfferMoney", &[Value::Float(10.0)], Some(trade.id), &empty).await.is_ok());
    assert_timeout(&db, trade.id).await;
    let bad = crate::services::keyval([("tradeContainerID", Value::Int(trade.id)), ("money", Value::list([Value::Float(0.0), Value::Float(0.0)])), ("tradeItems", Value::list([]))]);
    assert!(matches!(call(&mut ca, "MakeOffer", &[bad], Some(trade.id), &empty).await, Ok(Value::Bool(false))));
    assert_timeout(&db, trade.id).await;
    let rows = crate::services::items::load_items(&db, &sde, crate::dao::items::loading::ItemQuery::Location(trade.id)).await.unwrap();
    let manifest = crate::services::keyval([("tradeContainerID", Value::Int(trade.id)), ("money", Value::list([Value::Float(10.0), Value::Float(0.0)])), ("tradeItems", Value::list(rows.iter().map(|i| i.row())))]);
    assert!(matches!(call(&mut ca, "MakeOffer", &[manifest.clone()], Some(trade.id), &empty).await, Ok(Value::Bool(true))));
    assert_timeout(&db, trade.id).await;
    assert!(matches!(call(&mut cb, "MakeOffer", &[manifest], Some(trade.id), &empty).await, Ok(Value::Bool(true))));
    assert_no_timeout(&db, trade.id).await;
    assert!(call(&mut ca, "Abort", &[], Some(trade.id), &empty).await.is_ok());
    assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "done");
    assert_eq!(db.query_one("SELECT owner_id FROM game.items WHERE item_id = $1", &[&child]).await.unwrap().get::<_, i64>("owner_id"), b);
    assert_eq!(crate::dao::wallets::characters::balance(&db, a).await.unwrap(), crate::isk::IskAmount::from_cents(9_000).unwrap());
    assert_eq!(crate::dao::wallets::characters::balance(&db, b).await.unwrap(), crate::isk::IskAmount::from_cents(11_000).unwrap());
    for rx in [&mut ra, &mut rb] {
        let mut events = Vec::new(); while let Ok(event) = rx.try_recv() { events.push(event); }
        assert_eq!(events.iter().filter(|(name, _)| name == "OnTradeComplete").count(), 1);
        let accepted = events.iter().position(|(name, args)| name == "OnTradeOffer" && args.get(1) == Some(&Value::list([Value::Bool(true), Value::Bool(false)]))).expect("first acceptance event");
        let complete = events.iter().position(|(name, _)| name == "OnTradeComplete").unwrap();
        assert!(accepted < complete);
        assert!(!events.iter().any(|(name, args)| name == "OnTradeOffer" && args.get(1) == Some(&Value::list([Value::Bool(true), Value::Bool(true)]))));
        assert!(!events.iter().any(|(name, _)| name == "OnTradeCancel"));
    }
    assert!(call(&mut ca, "InitiateTrade", &[Value::Int(b)], None, &empty).await.is_ok());
    let disconnect_trade = trades::open_for(&db, a).await.unwrap().unwrap();
    assert_ne!(disconnect_trade.id, trade.id);
    assert!(call(&mut ca, "Add", &[Value::Int(stack), Value::Int(station)], Some(disconnect_trade.id), &empty).await.is_ok());
    assert_eq!(trades::items(&db, disconnect_trade.id).await.unwrap().len(), 1);
    assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id = $1", &[&stack]).await.unwrap().get::<_, i64>("location_id"), disconnect_trade.id);
    while rb.try_recv().is_ok() {}
    ca.leave_world().await.unwrap();
    assert!(!world.is_online(a));
    assert_eq!(trades::get(&db, disconnect_trade.id).await.unwrap().unwrap().state, "cancelled");
    assert_no_timeout(&db, disconnect_trade.id).await;
    assert!(trades::items(&db, disconnect_trade.id).await.unwrap().is_empty());
    let restored = db.query_one("SELECT owner_id, location_id, flag_id, quantity FROM game.items WHERE item_id = $1", &[&stack]).await.unwrap();
    assert_eq!(restored.get::<_, i64>("owner_id"), a);
    assert_eq!(restored.get::<_, i64>("location_id"), station);
    assert_eq!(restored.get::<_, i32>("flag_id"), infinity_core::consts::FLAG_HANGAR);
    assert_eq!(restored.get::<_, i64>("quantity"), 3);
    assert!(std::iter::from_fn(|| rb.try_recv().ok()).any(|(name, _)| name == "OnTradeCancel"));
    db.execute("DELETE FROM game.items WHERE owner_id = ANY($1)", &[&vec![a, b]]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id = ANY($1)", &[&vec![trade.id, disconnect_trade.id]]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&vec![a, b, third]]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
