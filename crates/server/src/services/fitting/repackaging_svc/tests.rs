//! Real-database coverage for station repackaging and confirmation.
//! Requires an initialized test database with SDE; refusal and teardown checks
//! compare only the row state under this fixture's explicit keys.
use std::sync::Arc;
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HANGAR, FLAG_RIG_FIRST, GROUP_STRATEGIC_CRUISER}};
use crate::{client::Client, db::Db, sde::Sde, services::{Call, CallError}, space::World};
use marshal::Value;

mod scenarios;
mod container_capacity;
mod ship_stance;

/// Scoped row state for refused repackaging: the supplied item keys with their
/// current descendants, this fixture's related hangar outputs of the given
/// types, and the supplied keys' exact insurance, dynamic, trade and industry rows.
async fn state(db: &Db, ids: &[i64], owner: i64, station: i64, types: &[i32]) -> (crate::dao::tests::ItemState, Vec<String>, Vec<String>, Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let items = crate::dao::tests::item_state(db, ids).await.unwrap();
    let outputs: Vec<String> = db.query("SELECT row_to_json(i)::text AS row FROM game.items i WHERE i.owner_id=$1 AND i.location_id=$2 AND i.flag_id=$3 AND i.type_id=ANY($4) ORDER BY i.item_id", &[&owner, &station, &FLAG_HANGAR, &types]).await.unwrap().iter().map(|row| row.get("row")).collect();
    let insurance: Vec<String> = db.query("SELECT row_to_json(i)::text AS row FROM game.insurance i WHERE i.ship_id=ANY($1) ORDER BY i.ship_id", &[&ids]).await.unwrap().iter().map(|row| row.get("row")).collect();
    let dynamic: Vec<String> = db.query("SELECT row_to_json(d)::text AS row FROM game.dynamic_items d WHERE d.item_id=ANY($1) ORDER BY d.item_id", &[&ids]).await.unwrap().iter().map(|row| row.get("row")).collect();
    let trades: Vec<String> = db.query("SELECT row_to_json(t)::text AS row FROM game.trades t WHERE t.trade_id=ANY($1) ORDER BY t.trade_id", &[&ids]).await.unwrap().iter().map(|row| row.get("row")).collect();
    let offered: Vec<String> = db.query("SELECT row_to_json(t)::text AS row FROM game.trade_items t WHERE t.item_id=ANY($1) OR t.trade_id=ANY($1) ORDER BY t.item_id", &[&ids]).await.unwrap().iter().map(|row| row.get("row")).collect();
    let jobs: Vec<String> = db.query("SELECT row_to_json(j)::text AS row FROM game.industry_jobs j WHERE j.blueprint_id=ANY($1) ORDER BY j.job_id", &[&ids]).await.unwrap().iter().map(|row| row.get("row")).collect();
    (items, outputs, insurance, dynamic, trades, offered, jobs)
}
async fn kills(db: &Db, ship: i64) -> Option<i64> {
    db.query_opt("SELECT player_kills FROM game.ship_kill_counts WHERE ship_item_id=$1", &[&ship]).await.unwrap().map(|row| row.get("player_kills"))
}


#[derive(Clone, Copy)]
struct Fixture { account: i64, owner: i64, hull: i64, rig: i64, station: i64 }

async fn create(db: &Db, station: i64, hull_type: i32, rig_type: i32) -> Fixture {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('repackage-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'repackage-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let hull = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,name,online) VALUES ($1,$2,$3,$4,1,true,'fixture',true) RETURNING item_id", &[&hull_type,&owner,&station,&FLAG_HANGAR]).await?.get("item_id");
        let rig = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&rig_type,&owner,&hull,&FLAG_RIG_FIRST]).await?.get("item_id");
        tx.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,1,42),($2,1,42)", &[&hull,&rig]).await?;
        tx.execute("INSERT INTO game.insurance (ship_id,owner_id,type_id,fraction,start_date,end_date) VALUES ($1,$2,$3,1,1,2)", &[&hull,&owner,&hull_type]).await?;
        Ok::<_, crate::db::Error>(Fixture { account, owner, hull, rig, station })
    }), |f, ()| async move { Ok(f) }).await.unwrap()
}

async fn cleanup(db: &Db, f: Fixture) {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.trade_items WHERE trade_id IN (SELECT trade_id FROM game.trades WHERE char_a=$1 OR char_b=$1)", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.trades WHERE char_a=$1 OR char_b=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.industry_jobs WHERE owner_id=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.pending_reloads WHERE owner_id=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.weapon_banks WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.insurance WHERE owner_id=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1 OR owner_id=-$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&f.account]).await?;
        Ok::<_, crate::db::Error>(())
    }), |(), ()| async move { Ok(()) }).await.unwrap();
}

/// Capture this fixture's owned keys before cleanup's first delete, then assert
/// no fixture row or feature sidecar survives under the retained keys.
async fn teardown(db: &Db, f: Fixture) {
    let fixture = crate::services::tests::fixture::Fixture { account: f.account, character: f.owner };
    let mut ids: Vec<i64> = db.query("SELECT item_id FROM game.items WHERE owner_id=$1 OR owner_id=$2", &[&f.owner, &(-f.owner)]).await.unwrap().iter().map(|row| row.get("item_id")).collect();
    ids.extend([f.hull, f.rig]); ids.sort_unstable(); ids.dedup();
    let rows = crate::services::tests::fixture::capture_fixture_rows(db, &[fixture], &ids).await.unwrap();
    cleanup(db, f).await;
    crate::services::tests::fixture::assert_fixture_removed(db, &rows).await.unwrap();
    assert!(db.query("SELECT 1 FROM game.items WHERE owner_id=$1", &[&(-f.owner)]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.insurance WHERE owner_id=$1 OR ship_id=ANY($2)", &[&f.owner, &ids]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.dynamic_items WHERE creator_id=$1 OR item_id=ANY($2)", &[&f.owner, &ids]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.trades WHERE char_a=$1 OR char_b=$1 OR trade_id=ANY($2)", &[&f.owner, &ids]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.trade_items WHERE owner_id=$1 OR item_id=ANY($2) OR trade_id=ANY($2)", &[&f.owner, &ids]).await.unwrap().is_empty());
    assert!(db.query("SELECT 1 FROM game.industry_jobs WHERE owner_id=$1 OR installer_id=$1 OR blueprint_id=ANY($2)", &[&f.owner, &ids]).await.unwrap().is_empty());
}

async fn request(c: &mut Client, f: Fixture, confirmation: Option<Value>) -> Result<Value, CallError> {
    let args = [Value::Dict(vec![(Value::Int(f.station), Value::list([Value::tuple([Value::Int(f.hull), Value::Int(f.station)])]))])];
    let kwargs = Value::Dict(confirmation.into_iter().map(|v| (Value::str("destroyable"), v)).collect());
    let call = Call { service: "repackagingSvc", method: "RepackageItems", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    super::dispatch(c, &call).await.expect("repackage dispatch")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn rig_confirmation_preserves_then_destroys() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let hull_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER) && t.fit_attribute(infinity_core::consts::dogma::ATTR_CAPACITY).is_some()).expect("ordinary hull with cargo capacity").type_id;
    let rig_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_RIG_SLOT) && crate::services::item_volume::item_volume(&sde, t.type_id, false, 7, None).is_ok_and(|v| v.is_finite() && v >= 0.0)).expect("rig type with valid volume").type_id;
    let f = create(&db, station, hull_type, rig_type).await;
    let world = World::new(db.clone(), sde.clone(), 1);
    db.execute("INSERT INTO game.ship_kill_counts (ship_item_id,player_kills) VALUES ($1,7)", &[&f.hull]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(infinity_wire::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "repackage-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        c.session.apply(vec![("charid", Value::Int(f.owner))]);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        let data = c.sde.clone();
        test_db.transaction(|tx, _: &mut ()| {
            let data = data.clone();
            Box::pin(async move {
                tx.query("SELECT item_id FROM game.items WHERE item_id=ANY($1) ORDER BY item_id FOR UPDATE", &[&&[f.hull,f.rig][..]]).await?;
                let hull = crate::services::items::load_items(tx, &data, crate::dao::items::loading::ItemQuery::Id(f.hull)).await?.pop().unwrap();
                let rig = crate::services::items::load_items(tx, &data, crate::dao::items::loading::ItemQuery::Id(f.rig)).await?.pop().unwrap();
                for (id, item, destroy) in [(f.hull, &hull, false), (f.rig, &rig, true)] {
                    for field in ["location_id", "quantity", "type_id"] {
                        tx.execute(&format!("UPDATE game.items SET {field}={field}+1 WHERE item_id=$1"), &[&id]).await?;
                        if destroy {
                            assert!(!crate::dao::items::fitting::destroy_upgrade(tx, item).await?);
                        } else {
                            assert_eq!(crate::dao::items::fitting::repackage(tx, item).await?, 0);
                        }
                        tx.execute(&format!("UPDATE game.items SET {field}={field}-1 WHERE item_id=$1"), &[&id]).await?;
                    }
                }
                Ok::<_, crate::db::Error>(())
            })
        }, |(), ()| async move { Ok(()) }).await.unwrap();
        test_db.execute("INSERT INTO game.item_service_life (item_id,damage) VALUES ($1,1)", &[&f.rig]).await.unwrap();
        let damaged = state(&test_db, &[f.hull, f.rig], f.owner, f.station, &[]).await;
        assert!(matches!(request(&mut c, f, Some(Value::Bool(true))).await, Err(CallError::User { .. })));
        assert_eq!(state(&test_db, &[f.hull, f.rig], f.owner, f.station, &[]).await, damaged);
        assert!(test_db.query_opt("SELECT item_id FROM game.item_service_life WHERE item_id=$1 AND damage=1", &[&f.rig]).await.unwrap().is_some());
        assert!(events.try_recv().is_err());
        test_db.execute("DELETE FROM game.item_service_life WHERE item_id=$1", &[&f.rig]).await.unwrap();
        let unchanged = state(&test_db, &[f.hull, f.rig], f.owner, f.station, &[]).await;
        for confirmation in [None, Some(Value::Bool(false)), Some(Value::str("true"))] {
            assert!(matches!(request(&mut c, f, confirmation).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES));
            assert_eq!(state(&test_db, &[f.hull, f.rig], f.owner, f.station, &[]).await, unchanged);
            assert!(events.try_recv().is_err());
        }
        // The guard-miss transaction and every refused confirmation kept the count.
        assert_eq!(kills(&test_db, f.hull).await, Some(7));
        let rig = crate::services::items::load_items(&test_db, &c.sde, crate::dao::items::loading::ItemQuery::Id(f.rig)).await.unwrap().pop().unwrap();
        let gone = crate::dao::items::loading::Item { location_id: infinity_core::consts::LOCATION_JUNKYARD, ..rig };
        assert!(matches!(request(&mut c, f, Some(Value::Bool(true))).await, Ok(Value::None)));
        let row = test_db.query_one("SELECT singleton,online,name FROM game.items WHERE item_id=$1", &[&f.hull]).await.unwrap();
        assert!(!row.get::<_, bool>("singleton"));
        assert!(!row.get::<_, bool>("online"));
        assert!(row.get::<_, Option<String>>("name").is_none());
        assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&f.rig]).await.unwrap().is_none());
        assert!(test_db.query("SELECT item_id FROM game.item_attributes WHERE item_id=ANY($1)", &[&&[f.hull,f.rig][..]]).await.unwrap().is_empty());
        assert!(test_db.query_opt("SELECT ship_id FROM game.insurance WHERE ship_id=$1", &[&f.hull]).await.unwrap().is_none());
        assert_eq!(kills(&test_db, f.hull).await, None);
        let root = crate::services::items::load_items(&test_db, &c.sde, crate::dao::items::loading::ItemQuery::Id(f.hull)).await.unwrap().pop().unwrap();
        let mut rig_notice = false;
        let mut root_notice = false;
        while let Ok((name, payload)) = events.try_recv() {
            if name != "OnItemChange" { continue; }
            if payload.first() == Some(&gone.row()) && payload.get(1) == Some(&Value::Dict(vec![(Value::Int(infinity_core::consts::IX_LOCATION_ID), Value::Int(f.hull))])) { rig_notice = true; }
            if payload.first() == Some(&root.row()) && payload.get(1) == Some(&Value::Dict(vec![(Value::Int(infinity_core::consts::IX_SINGLETON), Value::Bool(true))])) { root_notice = true; }
        }
        assert!(rig_notice && root_notice);
        assert!(matches!(request(&mut c, f, None).await, Ok(Value::None)));
        while events.try_recv().is_ok() {}
        scenarios::exercise(&mut c, &test_db, f, hull_type, rig_type, &mut events).await;
    }).await;
    teardown(&db, f).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn sealed_wrap_repackaging_refused_without_changes() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let hull_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER)).expect("ordinary hull").type_id;
    let rig_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_RIG_SLOT)).expect("rig type").type_id;
    let f = create(&db, station, hull_type, rig_type).await;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(infinity_wire::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "repackage-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        c.session.apply(vec![("charid", Value::Int(f.owner))]);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        scenarios::sealed_wrap(&mut c, &test_db, f, rig_type, &mut events).await;
    }).await;
    teardown(&db, f).await;
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn late_hull_update_failure_rolls_back_rig_and_notices() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let hull_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP) && t.group_id.map(i64::from) != Some(GROUP_STRATEGIC_CRUISER)).expect("ordinary hull").type_id;
    let rig_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_RIG_SLOT)).expect("rig type").type_id;
    let f = create(&db, station, hull_type, rig_type).await;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let second: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull_type,&f.owner,&f.station,&FLAG_HANGAR]).await.unwrap().get("item_id");
        let second_rig: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&rig_type,&f.owner,&second,&FLAG_RIG_FIRST]).await.unwrap().get("item_id");
        test_db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,1,42),($2,1,42)", &[&second,&second_rig]).await.unwrap();
        assert!(second > f.hull);
        test_db.execute("INSERT INTO game.ship_kill_counts (ship_item_id,player_kills) VALUES ($1,7)", &[&f.hull]).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(infinity_wire::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "repackage-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        c.session.apply(vec![("charid", Value::Int(f.owner))]);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        let unchanged = state(&test_db, &[f.hull, f.rig, second, second_rig], f.owner, f.station, &[]).await;
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut ready_tx = Some(ready_tx);
        let blocker = test_db.transaction(
            |tx, _: &mut ()| {
                let ready_tx = ready_tx.take().expect("barrier transaction retried");
                Box::pin(async move {
                    let pid: i32 = tx.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
                    tx.query_one("SELECT ship_id FROM game.insurance WHERE ship_id=$1 FOR UPDATE", &[&f.hull]).await?;
                    let _ = ready_tx.send(());
                    tokio::time::timeout(std::time::Duration::from_secs(15), async {
                        loop {
                            tx.query_one("SELECT pg_stat_clear_snapshot()", &[]).await?;
                            let blocked: bool = tx.query_one(
                                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND query ILIKE '%DELETE FROM game.insurance%') AS blocked",
                                &[&pid],
                            ).await?.get("blocked");
                            if blocked { break Ok::<_, crate::db::Error>(()); }
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }).await.expect("service did not block on insurance")?;
                    let child: i64 = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,false) RETURNING item_id", &[&rig_type,&f.owner,&second_rig,&infinity_core::consts::FLAG_CARGO]).await?.get("item_id");
                    Ok::<_, crate::db::Error>(child)
                })
            },
            |child, ()| async move { Ok(child) },
        );
        let service = async {
            ready_rx.await.expect("insurance barrier failed");
            scenarios::many(&mut c, f.station, &[f.hull, second], Some(Value::Bool(true))).await
        };
        let (child, outcome) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(blocker, service)
        }).await.expect("barrier or service timed out");
        let child = child.unwrap();
        assert!(matches!(outcome, Err(CallError::User { msg, dict }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY && dict == vec![("notify".into(), Value::unicode(infinity_core::messages::ITEM_NOT_AT_ORIGINAL_LOCATION))]));
        assert!(events.try_recv().is_err());
        assert_eq!(test_db.execute("DELETE FROM game.items WHERE item_id=$1 AND owner_id=$2", &[&child,&f.owner]).await.unwrap(), 1);
        assert_eq!(state(&test_db, &[f.hull, f.rig, second, second_rig], f.owner, f.station, &[]).await, unchanged);
        assert!(test_db.query_one("SELECT singleton FROM game.items WHERE item_id=$1", &[&f.hull]).await.unwrap().get::<_, bool>("singleton"));
        // The rolled-back repackage restored the kill-count row it deleted first.
        assert_eq!(kills(&test_db, f.hull).await, Some(7));
        for rig in [f.rig, second_rig] {
            assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&rig]).await.unwrap().is_some());
        }
    }).await;
    teardown(&db, f).await;
    result.unwrap();
}
