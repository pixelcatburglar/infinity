//! Trade wire validation and database reservation regressions.
use super::session::{matches_manifest, money};
use crate::dao::trades::{Trade, TradeItem};
use crate::services::{item_descriptor, keyval};
use marshal::Value;
#[path = "protocol_tests.rs"]
mod protocol_tests;
mod wrap;

fn trade() -> Trade {
    Trade { id: 100, station: 1, a: 2, b: 3, money_a: 1.25, money_b: 0.0, accepted_a: false, accepted_b: false, state: "open".into(), updated_at: 0 }
}
fn manifest(rows: Vec<Value>) -> Value {
    keyval([("tradeContainerID", Value::Int(100)), ("money", Value::list([Value::Float(1.25), Value::Float(0.0)])), ("tradeItems", Value::list(rows))])
}
fn row(id: i64) -> Value {
    item_descriptor().row(&[Value::Int(id), Value::Int(1), Value::Int(2), Value::Int(100), Value::Int(4), Value::Int(1), Value::Int(1), Value::Int(1), Value::None])
}

#[test]
fn offer_money_rejects_nonfinite_and_rounds() {
    assert_eq!(money(&Value::Float(1.234)), Some(1.23));
    assert_eq!(money(&Value::Float(-1.0)), None);
    assert_eq!(money(&Value::Float(f64::NAN)), None);
    assert_eq!(money(&Value::Float(f64::INFINITY)), None);
    assert_eq!(money(&Value::Float(f64::MAX)), None);
}

#[test]
fn manifest_requires_exact_valid_dbrow_ids() {
    let roots = [TradeItem { id: 42, owner: 2, origin: 1, flag: 4 }];
    assert!(matches_manifest(&manifest(vec![row(42)]), &trade(), &roots));
    assert!(!matches_manifest(&manifest(vec![row(43)]), &trade(), &roots));
    assert!(!matches_manifest(&manifest(vec![row(42), Value::None]), &trade(), &roots));
    assert!(!matches_manifest(&manifest(vec![row(42), row(42)]), &trade(), &roots));
    assert!(!matches_manifest(&manifest(vec![]), &trade(), &roots));
    let invalid_money = keyval([("tradeContainerID", Value::Int(100)), ("money", Value::list([Value::Float(f64::NAN), Value::Float(0.0)])), ("tradeItems", Value::list([row(42)]))]);
    assert!(!matches_manifest(&invalid_money, &trade(), &roots));
    let wrong_container = keyval([("tradeContainerID", Value::Int(101)), ("money", Value::list([Value::Float(1.25), Value::Float(0.0)])), ("tradeItems", Value::list([row(42)]))]);
    assert!(!matches_manifest(&wrong_container, &trade(), &roots));
}

#[test]
fn mutation_resets_both_acceptances() {
    let mut trade = trade();
    trade.accepted_a = true;
    trade.accepted_b = true;
    let mut outbox = crate::db::outbox::Outbox::default();
    super::trade_mgr::reset(&mut outbox, &mut trade);
    assert!(!trade.accepted_a && !trade.accepted_b);
}

#[tokio::test]
#[ignore]
async fn reservation_lineage_batch_and_closed_trade() {
    let cfg = crate::config::tests::test_database();
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let trade = crate::dao::trades::create(&tx, 1, 2, 3, 1).await.unwrap();
    let root = crate::dao::items::transfers::insert(&tx, 1, 2, trade.id, 4, 1).await.unwrap();
    let nested = crate::dao::items::transfers::insert(&tx, 1, 2, root, 4, 2).await.unwrap();
    let free = crate::dao::items::transfers::insert(&tx, 1, 2, 1, 4, 1).await.unwrap();
    assert_eq!(crate::dao::trades::reserved(&tx, &[nested, free]).await.unwrap(), vec![nested]);
    assert_eq!(crate::dao::trades::reserved(&tx, &[trade.id, root]).await.unwrap(), vec![trade.id, root]);
    let nested_item = crate::dao::items::loading::Item { item_id: nested, type_id: 1, owner_id: 2, location_id: root, flag_id: 4, quantity: 2, singleton: false, group_id: 1, category_id: 1, custom_info: None };
    assert!(crate::services::inventory::stacks::split_stack(&tx, &nested_item, 1, 2, root, 4, false).await.is_err());
    tx.execute("UPDATE game.trades SET state = 'cancelled' WHERE trade_id = $1", &[&trade.id]).await.unwrap();
    assert!(crate::dao::trades::reserved(&tx, &[nested]).await.unwrap().is_empty());
    tx.execute("UPDATE game.items SET location_id = $2 WHERE item_id = $1", &[&root, &nested]).await.unwrap();
    assert!(crate::dao::trades::reserved(&tx, &[root]).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn settlement_abort_and_rollback_preserve_item_trees() {
    use crate::{config::deployment::DatabaseConfig, dao::{items::transfers, trades, wallets::characters}, db::{outbox::{Effect, Notification, Outbox}, Db}, sde::Sde};
    use crate::progress::{prepare_batch, AfterCommit, Registry};
    let cfg = DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() };
    let db = Db::connect(&cfg).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let type_id = sde.types.iter().next().expect("SDE types required").type_id;
    let (mut client, connection) = cfg.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let account: i64 = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES (nextval('game.accounts_user_id_seq')::text, ''::bytea, 0) RETURNING user_id", &[]).await.unwrap().get("user_id");
    let a: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, 1, 100) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let b: i64 = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id, station_id, balance) VALUES ($1, nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1, 1, 100) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let mut trade = trades::create(&tx, 1, a, b, 1).await.unwrap();
    let root = transfers::insert(&tx, type_id, a, trade.id, 4, 1).await.unwrap();
    let child = transfers::insert(&tx, type_id, a, root, 5, 1).await.unwrap();
    let leaf = transfers::insert(&tx, type_id, a, child, 6, 1).await.unwrap();
    trades::add_item(&tx, trade.id, &TradeItem { id: root, owner: a, origin: 1, flag: 4 }).await.unwrap();
    trade.money_a = 101.0;
    trade.accepted_a = true;
    trade.accepted_b = true;
    trades::update(&tx, &trade, 1).await.unwrap();
    let mut rejected: Outbox<super::TradeEffect> = Outbox::default();
    assert!(matches!(super::settle::complete(&tx, &sde, &mut rejected, &mut trade).await, Ok(super::settle::Settled::Underfunded)));
    let stored = trades::get(&tx, trade.id).await.unwrap().unwrap();
    assert!(!stored.accepted_a && !stored.accepted_b);
    assert_eq!(stored.state, "open");
    let job_id = format!("trade-timeout:{}", trade.id);
    let due: i64 = tx.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap().get("due_at");
    assert_eq!(due, stored.updated_at + crate::client::filetime_ticks_from_seconds(crate::config::game().trade.idle_timeout_secs));
    assert_eq!(characters::balance(&tx, a).await.unwrap(), crate::isk::IskAmount::from_cents(10_000).unwrap());
    assert_eq!(characters::balance(&tx, b).await.unwrap(), crate::isk::IskAmount::from_cents(10_000).unwrap());
    let before = tx.query("SELECT owner_id, location_id FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![root, child, leaf]]).await.unwrap();
    assert_eq!(before.len(), 3);
    assert!(before.iter().all(|row| row.get::<_, i64>("owner_id") == a));
    assert_eq!(before[0].get::<_, i64>("location_id"), trade.id);
    assert_eq!(before[1].get::<_, i64>("location_id"), root);
    assert_eq!(before[2].get::<_, i64>("location_id"), child);
    let resets: Vec<_> = rejected.into_entries().into_iter().filter_map(|entry| match entry {
        Effect::Notification(Notification::Pilot { char_id, event, args }) if event == "OnTradeOfferReset" => Some((char_id, args)),
        _ => None,
    }).collect();
    assert_eq!(resets.len(), 2);
    assert!(resets.iter().any(|(id, _)| *id == a));
    assert!(resets.iter().any(|(id, _)| *id == b));
    assert!(resets.iter().all(|(_, args)| args == &vec![Value::Int(trade.id), Value::Bool(false)]));
    trade.money_a = 10.0;
    let mut outbox: Outbox<super::TradeEffect> = Outbox::default();
    assert!(matches!(super::settle::complete(&tx, &sde, &mut outbox, &mut trade).await, Ok(super::settle::Settled::Done)));
    assert!(tx.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&job_id]).await.unwrap().is_none());
    assert_eq!(characters::balance(&tx, a).await.unwrap(), crate::isk::IskAmount::from_cents(9_000).unwrap());
    assert_eq!(characters::balance(&tx, b).await.unwrap(), crate::isk::IskAmount::from_cents(11_000).unwrap());
    let invocation = (u128::from(crate::handshake::random_u64()) << u64::BITS) | u128::from(crate::handshake::random_u64());
    let inserted = prepare_batch(&mut outbox, &tx, invocation).await.unwrap();
    assert!(!inserted.is_empty());
    let _publication = outbox.after_commit(&Registry::default(), &inserted);
    let events: Vec<_> = outbox.into_entries().into_iter().filter_map(|entry| match entry {
        Effect::Notification(Notification::Pilot { char_id, event, args }) => Some((char_id, event, args)),
        _ => None,
    }).collect();
    assert_eq!(events.iter().filter(|(_, event, _)| event == "OnTradeComplete").count(), 2);
    assert!(events.iter().any(|(id, event, _)| *id == a && event == "OnTradeComplete"));
    assert!(events.iter().any(|(id, event, _)| *id == b && event == "OnTradeComplete"));
    assert!(!events.iter().any(|(_, event, args)| event == "OnTradeOffer" && args.get(1) == Some(&Value::list([Value::Bool(true), Value::Bool(true)]))));
    let rows = tx.query("SELECT item_id, owner_id, location_id FROM game.items WHERE item_id = ANY($1) ORDER BY item_id", &[&vec![root, child, leaf]]).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.get::<_, i64>("owner_id") == b));
    assert_eq!(rows[0].get::<_, i64>("location_id"), 1);
    assert_eq!(rows[1].get::<_, i64>("location_id"), root);
    assert_eq!(rows[2].get::<_, i64>("location_id"), child);
    let mut cancelled = trades::create(&tx, 1, a, b, 1).await.unwrap();
    let restored = transfers::insert(&tx, type_id, a, cancelled.id, 7, 1).await.unwrap();
    trades::add_item(&tx, cancelled.id, &TradeItem { id: restored, owner: a, origin: 123, flag: 9 }).await.unwrap();
    assert!(super::timeout::refresh(&tx, &mut Outbox::default(), &cancelled).await.is_ok());
    assert!(super::settle::abort(&tx, &sde, &mut Outbox::default(), &mut cancelled, false).await.is_ok());
    assert!(tx.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("trade-timeout:{}", cancelled.id)]).await.unwrap().is_none());
    let row = tx.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id = $1", &[&restored]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), 123);
    assert_eq!(row.get::<_, i32>("flag_id"), 9);
    let mut broken = trades::create(&tx, 1, a, b, 1).await.unwrap();
    let valid = transfers::insert(&tx, type_id, a, broken.id, 4, 1).await.unwrap();
    trades::add_item(&tx, broken.id, &TradeItem { id: valid, owner: a, origin: 123, flag: 9 }).await.unwrap();
    trades::add_item(&tx, broken.id, &TradeItem { id: i64::MAX, owner: a, origin: 123, flag: 9 }).await.unwrap();
    let savepoint = tx.savepoint("broken_trade").await.unwrap();
    assert!(super::settle::abort(&savepoint, &sde, &mut Outbox::default(), &mut broken, false).await.is_err());
    savepoint.rollback().await.unwrap();
    let unchanged: i64 = tx.query_one("SELECT location_id FROM game.items WHERE item_id = $1", &[&valid]).await.unwrap().get("location_id");
    assert_eq!(unchanged, broken.id);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn startup_recovery_restores_open_trade_without_notifications() {
    use std::sync::Arc;
    use crate::{config::deployment::DatabaseConfig, dao::{items::transfers, trades}, db::Db, sde::Sde, space::World};
    let cfg = DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() };
    let db = Db::connect(&cfg).await.unwrap();
    assert!(trades::open(&db).await.unwrap().is_empty(), "recovery test requires no pre-existing open trades");
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let restricted = sde.types.iter().find(|item| !sde.is_tradable(item.type_id)).expect("restricted type required");
    assert!(!sde.is_tradable(restricted.type_id));
    let type_id = sde.types.iter().next().expect("SDE types required").type_id;
    let trade = trades::create(&db, 1, 2, 3, 1).await.unwrap();
    let root = transfers::insert(&db, type_id, 2, trade.id, 7, 1).await.unwrap();
    trades::add_item(&db, trade.id, &TradeItem { id: root, owner: 2, origin: 123, flag: 9 }).await.unwrap();
    assert!(super::timeout::refresh(&db, &mut crate::db::outbox::Outbox::default(), &trade).await.is_ok());
    let world = World::new(db.clone(), sde.clone(), 1);
    assert!(super::recover(&db, &world, sde.clone()).await.is_ok());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("trade-timeout:{}", trade.id)]).await.unwrap().is_none());
    let row = db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id = $1", &[&root]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), 123);
    assert_eq!(row.get::<_, i32>("flag_id"), 9);
    assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "cancelled");
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&root]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&trade.id]).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn schema_restart_does_not_reuse_trade_container_ids() {
    use crate::{config::deployment::DatabaseConfig, dao::trades, db::Db};
    let cfg = DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() };
    let db = Db::connect(&cfg).await.unwrap();
    let trade = trades::create(&db, 1, 2, 3, 1).await.unwrap();
    let restart = DatabaseConfig { schema_on_start: true, ..cfg };
    let restarted = Db::connect(&restart).await.unwrap();
    let id: i64 = restarted.query_one("SELECT nextval('game.items_item_id_seq') AS id", &[]).await.unwrap().get("id");
    assert!(id > trade.id);
    restarted.execute("DELETE FROM game.trades WHERE trade_id = $1", &[&trade.id]).await.unwrap();
}
