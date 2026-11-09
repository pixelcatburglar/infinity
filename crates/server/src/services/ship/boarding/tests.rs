//! Production Board regressions using initialized game data and the real SDE.
//! Mirrors clientDogmaLocation.MakeShipActive; no foreign custody is modelled.
mod refusals;
mod transitions;

use std::sync::Arc;
use infinity_core::{consts::{CATEGORY_SHIP, FLAG_HANGAR}};
use crate::{client::Client, db::Db, sde::Sde, space::World};
use infinity_core::consts::dogma::{ATTR_REQUIRED_SKILLS, ATTR_REQUIRED_SKILL_LEVELS, ATTR_SKILL_TIME_CONSTANT};
use marshal::Value;

async fn fingerprint(db: &Db) -> Vec<(String, Vec<(String, String)>)> {
    let mut result = Vec::new();
    for (table, key) in [("accounts", "user_id::text"), ("characters", "character_id::text"),
        ("items", "item_id::text"), ("item_attributes", "item_id::text || ':' || attribute_id::text"),
        ("skills", "character_id::text || ':' || type_id::text")] {
        let rows = db.query(&format!("SELECT {key} AS key, md5(row_to_json(t)::text) AS hash FROM game.{table} t ORDER BY key"), &[]).await.unwrap()
            .iter().map(|row| (row.get("key"), row.get("hash"))).collect();
        result.push((table.to_owned(), rows));
    }
    result
}

async fn client(db: Db, sde: Arc<Sde>, world: Arc<World>, account: i64) -> Client {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let chat = crate::chat::Hub::new("localhost", db.clone(), world.clone());
    Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), db, sde,
        chat, world, crate::handshake::Login {
            user_id: account, user_name: "boarding-fixture".into(), role: 0, client_id: account, session_id: account, language_id: "EN".into(),
        })
}

async fn train(db: &Db, sde: &Sde, owner: i64, kind: i32) {
    let base = |attr| crate::services::fit::base_attribute(sde.types.get(kind), sde.dogma.attribute(attr), attr);
    for (&skill_attr, level_attr) in ATTR_REQUIRED_SKILLS.iter().zip(ATTR_REQUIRED_SKILL_LEVELS) {
        let skill = base(skill_attr) as i32;
        let level = base(level_attr) as i32;
        if skill <= 0 || level <= 0 { continue; }
        let rank = sde.types.get(skill).and_then(|t| t.fit_attribute(ATTR_SKILL_TIME_CONSTANT)).expect("required skill rank");
        let mut points = 1_i64;
        while crate::services::skills::formulas::level_from_points(points, rank) < level {
            points = points.checked_mul(2).expect("attainable skill level");
        }
        db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,$4) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level),skill_points=GREATEST(game.skills.skill_points,EXCLUDED.skill_points)", &[&owner, &skill, &level, &points]).await.unwrap();
    }
}

fn fourtuple(value: Value) {
    let Value::Tuple(fields) = value else { panic!("expected ship state tuple") };
    assert_eq!(fields.len(), 4);
}

async fn board_call(c: &mut Client, target: i64, context: Value) -> crate::services::CallResult {
    let args = [Value::Int(target), context];
    let kwargs = Value::None;
    let call = crate::services::Call { service: "ship", method: "Board", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
    crate::services::ship::dispatch(c, &call).await.expect("ship Board routed")
}

async fn cleanup(db: &Db, account: i64, owner: i64) {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&owner]).await?;
        tx.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

async fn fixture(db: &Db, sde: &Sde, system: i64, station: Option<i64>) -> (i64, i64) {
    let row = sde.map.system(system).expect("SDE system");
    let constellation = row.constellation_id.expect("SDE system constellation");
    let region = row.region_id.expect("SDE system region");
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('boarding-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id,solar_system_id,station_id,constellation_id,region_id) VALUES ($1,'boarding-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1,$2,$3,$4,$5) RETURNING character_id", &[&account,&system,&station,&constellation,&region]).await?.get("character_id");
        Ok::<_, crate::db::Error>((account, owner))
    }), |v, ()| async move { Ok(v) }).await.unwrap()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn station_board_switch_refresh_and_refusals() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().find(|s| s.solar_system_id.is_some()).expect("SDE station");
    let (station, system) = (station.station_id, station.solar_system_id.unwrap());
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true)).map(|t| t.type_id).min().expect("SDE ship");
    let before = fingerprint(&db).await;
    let (account, owner) = fixture(&db, &sde, system, Some(station)).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let old = crate::dao::items::creation::rookie_ship(&test_db, kind, owner, station, FLAG_HANGAR).await.unwrap();
        let target = crate::dao::items::creation::rookie_ship(&test_db, kind, owner, station, FLAG_HANGAR).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner,&old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let mut c = client(test_db.clone(), sde, world, account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("stationid", Value::Int(station)), ("solarsystemid2", Value::Int(system))]);
        c.world.register_pilot(owner, c.notify_tx.clone());
        c.world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        let snapshot = fingerprint(&test_db).await;
        let queued = c.outbox.len();
        for (target, context) in [(target, Value::Int(station)), (target, Value::None), (i64::MAX, Value::Int(old))] {
            assert!(board_call(&mut c, target, context).await.is_err());
            assert_eq!(fingerprint(&test_db).await, snapshot);
            assert_eq!(c.outbox.len(), queued);
            assert_eq!(c.session.int("shipid"), Some(old));
            assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(old));
        }
        fourtuple(board_call(&mut c, target, Value::Int(old)).await.ok().expect("station Board"));
        assert_eq!(c.session.int("shipid"), Some(target));
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(target));
        let switched = fingerprint(&test_db).await;
        fourtuple(board_call(&mut c, target, Value::Int(target)).await.ok().expect("current target refresh"));
        assert_eq!(fingerprint(&test_db).await, switched);
        assert_eq!(c.session.int("shipid"), Some(target));
    }).await;
    cleanup(&db, account, owner).await;
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn gm_spawn_then_board_preserves_old_hull_across_restart() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let system = sde.map.systems().next().expect("SDE system").solar_system_id;
    let kind = sde.types.iter().filter(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true))
        .filter(|t| crate::services::ship::balls::radius(&sde, t.type_id).is_ok()).map(|t| t.type_id).min().expect("SDE ship");
    let before = fingerprint(&db).await;
    let (account, owner) = fixture(&db, &sde, system, None).await;
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let pos = [100_000_000.0, 100_000_000.0, 100_000_000.0];
        let old = crate::dao::items::creation::positioned(&test_db, kind, owner, system, pos).await.unwrap();
        test_db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&owner,&old]).await.unwrap();
        train(&test_db, &sde, owner, kind).await;
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let row = crate::dao::items::space_ships::FreeShip { item_id: old, type_id: kind, owner_id: owner, corporation_id: 1, alliance_id: 0, pos };
        let pilot = crate::services::ship::balls::piloted(&test_db, sde.clone(), &row, owner, 0.0).await.unwrap();
        world.system(system).await.unwrap();
        world.with(system, move |s| pilot.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let mut c = client(test_db.clone(), sde.clone(), world.clone(), account).await;
        c.session.apply(vec![("userid", Value::Int(account)), ("charid", Value::Int(owner)), ("shipid", Value::Int(old)), ("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        world.register_pilot(owner, c.notify_tx.clone());
        world.set_online_session(owner, infinity_core::session::OnlineSession::from_session(&c.session));
        world.observe(system, c.login.client_id, old, c.destiny_tx.clone()).await.unwrap();
        let target = crate::services::fitting::ship::gm_spawn_ship(&mut c, kind).await.unwrap();
        assert_eq!(c.session.int("shipid"), Some(old));
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(old));
        assert_eq!(world.ball(system, target).await.unwrap().char_id, None);
        let snapshot = fingerprint(&test_db).await;
        let queued = c.outbox.len();
        assert!(super::board(&mut c, target, Some(target)).await.is_err());
        assert!(super::board(&mut c, i64::MAX, Some(old)).await.is_err());
        assert_eq!(fingerprint(&test_db).await, snapshot);
        assert_eq!(c.outbox.len(), queued);
        assert_eq!(world.with(system, move |s| s.pilot_ball(owner).map(|b| b.id)).await.unwrap(), Some(old));
        fourtuple(super::board(&mut c, target, Some(old)).await.ok().expect("space Board"));
        assert_eq!(c.session.int("shipid"), Some(target));
        assert_eq!(crate::dao::characters::selection::ship_id(&test_db, owner).await.unwrap(), Some(target));
        assert_eq!(world.with(system, move |s| s.pilot_ball(owner).map(|b| b.id)).await.unwrap(), Some(target));
        assert_eq!(world.ball(system, old).await.unwrap().char_id, None);
        assert_eq!(world.ball(system, target).await.unwrap().char_id, Some(owner));
        let restored = World::new(test_db.clone(), sde, 1);
        restored.system(system).await.unwrap();
        assert_eq!(restored.ball(system, old).await.unwrap().char_id, None);
        assert_eq!(restored.with(system, move |s| s.persistent_balls().filter(|b| b.id == old).count()).await, Some(1));
    }).await;
    cleanup(&db, account, owner).await;
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}
