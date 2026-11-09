//! Service-path regression for logical singleton quantities in trade staging.
use std::sync::Arc;
use marshal::Value;
use crate::{client::Client, config::{Config, deployment::DatabaseConfig}, conn::Conn, dao::{items::transfers, trades}, db::Db, handshake::Login, sde::Sde, services::Call, space::World};

#[tokio::test]
#[ignore]
async fn singleton_encoded_quantity_stages_as_one() {
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("station required").station_id;
    let ty = sde.types.iter().find(|t| sde.is_tradable(t.type_id)).expect("tradable type required").type_id;
    let world = World::new(db.clone(), sde.clone(), 1);
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let a: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let b: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, $2, 100) RETURNING character_id", &[&account, &station]).await.unwrap().get("character_id");
    let trade = trades::create(&db, station, a, b, 1).await.unwrap();
    let item = transfers::insert(&db, ty, a, station, infinity_core::consts::FLAG_HANGAR, -12345).await.unwrap();
    let control = transfers::insert(&db, ty, a, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&control]).await.unwrap();
    db.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&item]).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, addr) = listener.accept().await.unwrap();
    let login = Login { user_id: account, user_name: account.to_string(), role: 0, client_id: a, session_id: a, language_id: "EN".into() };
    let chat = crate::chat::Hub::new("localhost", db.clone(), world.clone());
    let mut c = Client::new(Conn::new(stream, addr), Arc::new(Config::default()), db.clone(), sde, chat, world, login);
    c.session.apply(vec![("charid", Value::Int(a)), ("stationid", Value::Int(station)), ("shipid", Value::Int(0))]);
    let args = [Value::Int(item), Value::Int(station)];
    let kwargs = Value::dict([("qty", Value::Int(1))]);
    let invalid = Value::dict([("qty", Value::Int(2))]);
    let bind = Value::Int(trade.id);
    let rejected = super::super::route(&mut c, &Call { service: "trademgr", method: "Add", args: &args, kwargs: &invalid, bound: true, bind_params: Some(&bind) }).await;
    assert!(rejected.is_err());
    let result = super::super::route(&mut c, &Call { service: "trademgr", method: "Add", args: &args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await;
    assert!(result.is_ok(), "singleton staging failed: {result:?}");
    let control_args = [Value::Int(control), Value::Int(station)];
    let result = super::super::route(&mut c, &Call { service: "trademgr", method: "Add", args: &control_args, kwargs: &kwargs, bound: true, bind_params: Some(&bind) }).await;
    assert!(result.is_ok(), "singleton staging failed: {result:?}");
    let row = db.query_one("SELECT location_id, quantity FROM game.items WHERE item_id = $1", &[&item]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), trade.id);
    assert_eq!(row.get::<_, i64>("quantity"), -12345);
    db.execute("DELETE FROM game.trade_items WHERE trade_id = $1", &[&trade.id]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1)", &[&vec![item, control]]).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id = $1", &[&format!("trade-timeout:{}", trade.id)]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&trade.id]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = ANY($1)", &[&vec![a, b]]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
}
