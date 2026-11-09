//! Database regressions for the real dogmaIM grant dispatch.
mod bonus;
mod ships;
mod refusals;
mod rollback;

use super::{grant_core, Grant, OnlineChanged, Outbox, Identity, online};
use std::sync::Arc;
use infinity_core::consts::FLAG_HANGAR;
use crate::{client::Client, db::Db, sde::Sde, space::World};
use crate::services::{Call, CallResult, tests::fixture};
use marshal::Value;

async fn database() -> (Db, Arc<Sde>) {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    (db, sde)
}

async fn client(db: Db, sde: Arc<Sde>, f: fixture::Fixture, station: i64, system: i64, ship: i64) -> Client {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let chat = crate::chat::Hub::new("localhost", db.clone(), world.clone());
    let mut c = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), db, sde, chat, world,
        crate::handshake::Login { user_id: f.account, user_name: "grant-fixture".into(), role: 0, client_id: f.account, session_id: f.account, language_id: "EN".into() });
    c.session.apply(vec![("userid", Value::Int(f.account)), ("charid", Value::Int(f.character)), ("shipid", Value::Int(ship)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
    c.world.register_pilot(f.character, c.notify_tx.clone());
    c.world.set_online_session(f.character, infinity_core::session::OnlineSession::from_session(&c.session));
    c
}

async fn setup(db: &Db, sde: &Sde, race: i32, kind: i32) -> (fixture::Fixture, i64, i64, i64) {
    let f = fixture::create(db).await.unwrap();
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some()).expect("NPC station");
    if let Some(target) = sde.characters.race(i64::from(race)).and_then(|r| r.ship_type_id) {
        let base = |attr| crate::services::fit::base_attribute(sde.types.get(target), sde.dogma.attribute(attr), attr);
        for (&skill_attr, level_attr) in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS.iter().zip(infinity_core::consts::dogma::ATTR_REQUIRED_SKILL_LEVELS) {
            let skill = base(skill_attr) as i32;
            let level = base(level_attr) as i32;
            if skill <= 0 || level <= 0 { continue; }
            let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT)).expect("required skill rank");
            let mut points = 1_i64;
            while crate::services::skills::formulas::level_from_points(points, rank) < level { points = points.checked_mul(2).unwrap(); }
            db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level),skill_points=GREATEST(game.skills.skill_points,EXCLUDED.skill_points)", &[&f.character, &skill, &level, &points]).await.unwrap();
        }
    }
    let (station, system) = (station.station_id, station.solar_system_id.unwrap());
    let ship = crate::dao::items::creation::rookie_ship(db, kind, f.character, station, FLAG_HANGAR).await.unwrap();
    db.execute("UPDATE game.characters SET race_id=$2, station_id=$3, solar_system_id=$4, ship_id=$5 WHERE character_id=$1", &[&f.character, &race, &station, &system, &ship]).await.unwrap();
    (f, station, system, ship)
}

async fn cleanup(db: &Db, f: fixture::Fixture) {
    db.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&f.character]).await.unwrap();
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(db, f).await.unwrap();
}

async fn rpc(c: &mut Client, method: &str, args: &[Value], kwargs: &Value) -> CallResult {
    crate::services::dispatch(c, &Call { service: "dogmaIM", method, args, kwargs, bound: false, bind_params: None }).await
}

async fn snapshot(db: &Db, owner: i64) -> (String, Option<i64>, Option<i64>) {
    let row = db.query_one("SELECT COALESCE((SELECT string_agg(row_to_json(i)::text, '' ORDER BY item_id) FROM game.items i WHERE owner_id=$1),'') AS items, ship_id, newbie_bonus_claimed_at FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    (row.get("items"), row.get("ship_id"), row.get("newbie_bonus_claimed_at"))
}

fn item_packets(c: &Client, session_expected: bool) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut session = false;
    for packet in &c.outbox {
        let packet = crate::macho::Packet::from_value(&marshal::loads(&marshal::dumps(&packet.to_value())).unwrap()).unwrap();
        if packet.command == crate::macho::cmd::SESSIONCHANGENOTIFICATION {
            assert!(!session);
            session = true;
        }
        if let crate::macho::Address::Broadcast { broadcast_id, .. } = &packet.destination {
            if broadcast_id.as_text().as_deref() == Some("OnItemChange") {
                assert!(!session, "inventory must precede session change");
                let envelope = packet.body[0].items().unwrap();
                let payload = marshal::loads(envelope[1].as_bytes().unwrap()).unwrap();
                let args = payload.items().unwrap()[1].items().unwrap();
                assert_eq!(args.len(), 3);
                assert_eq!(args[1], Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(0))]));
                assert_eq!(args[2], Value::None);
                rows.push(args[0].clone());
            }
        }
    }
    assert_eq!(session, session_expected);
    rows
}
