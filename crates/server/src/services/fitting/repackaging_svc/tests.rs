//! Real-database coverage for station repackaging and confirmation.
//! Requires a dedicated quiet test database: whole-database fingerprints intentionally prove no side effects anywhere.
use std::sync::Arc;
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HANGAR, FLAG_RIG_FIRST, GROUP_STRATEGIC_CRUISER}};
use crate::{client::Client, db::Db, sde::Sde, services::{Call, CallError}, space::World};
use marshal::Value;

mod scenarios;
mod container_capacity;

async fn fingerprint(db: &Db) -> Vec<(String, String)> {
    let tables = [
        "accounts",
        "characters",
        "items",
        "item_attributes",
        "dynamic_items",
        "insurance",
        "module_charges",
        "pending_reloads",
        "weapon_banks",
        "trades",
        "trade_items",
        "industry_jobs",
    ];
    let mut hashes = Vec::new();
    for table in tables {
        let name = table.to_string();
        let sql = format!("SELECT count(*)::text || ':' || md5(COALESCE(string_agg(md5(row_to_json(t)::text), '' ORDER BY md5(row_to_json(t)::text)), '')) AS fingerprint FROM game.\"{}\" t", name.replace('"', "\"\""));
        hashes.push((name, db.query_one(&sql, &[]).await.unwrap().get("fingerprint")));
    }
    hashes
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
    let before = fingerprint(&db).await;
    let f = create(&db, station, hull_type, rig_type).await;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "repackage-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
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
        let unchanged = fingerprint(&test_db).await;
        for confirmation in [None, Some(Value::Bool(false)), Some(Value::str("true"))] {
            assert!(matches!(request(&mut c, f, confirmation).await, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CONFIRM_REPACKAGE_SOMETHING_WITH_UPGRADES));
            assert_eq!(fingerprint(&test_db).await, unchanged);
            assert!(events.try_recv().is_err());
        }
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
    cleanup(&db, f).await;
    assert_eq!(fingerprint(&db).await, before);
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
    let before = fingerprint(&db).await;
    let f = create(&db, station, hull_type, rig_type).await;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "repackage-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        c.session.apply(vec![("charid", Value::Int(f.owner))]);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        scenarios::sealed_wrap(&mut c, &test_db, f, rig_type, &mut events).await;
    }).await;
    cleanup(&db, f).await;
    assert_eq!(fingerprint(&db).await, before);
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
    let before = fingerprint(&db).await;
    let f = create(&db, station, hull_type, rig_type).await;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let second: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull_type,&f.owner,&f.station,&FLAG_HANGAR]).await.unwrap().get("item_id");
        let second_rig: i64 = test_db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&rig_type,&f.owner,&second,&FLAG_RIG_FIRST]).await.unwrap().get("item_id");
        test_db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,1,42),($2,1,42)", &[&second,&second_rig]).await.unwrap();
        assert!(second > f.hull);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let mut c = Client::new(crate::conn::Conn::new(stream, address), Arc::new(crate::config::Config::default()), test_db.clone(), sde, crate::chat::Hub::new("localhost", test_db.clone(), world.clone()), world, crate::handshake::Login { user_id: f.account, user_name: "repackage-fixture".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
        c.session.apply(vec![("charid", Value::Int(f.owner))]);
        let (notify, mut events) = tokio::sync::mpsc::unbounded_channel();
        c.notify_tx = notify;
        let unchanged = fingerprint(&test_db).await;
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
        assert_eq!(fingerprint(&test_db).await, unchanged);
        assert!(test_db.query_one("SELECT singleton FROM game.items WHERE item_id=$1", &[&f.hull]).await.unwrap().get::<_, bool>("singleton"));
        for rig in [f.rig, second_rig] {
            assert!(test_db.query_opt("SELECT item_id FROM game.items WHERE item_id=$1", &[&rig]).await.unwrap().is_some());
        }
    }).await;
    cleanup(&db, f).await;
    assert_eq!(fingerprint(&db).await, before);
    result.unwrap();
}
