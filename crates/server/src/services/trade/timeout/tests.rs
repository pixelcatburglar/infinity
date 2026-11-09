//! Real database regressions for durable trade idle expiry; run serially.
use super::*;
use crate::{config::deployment::DatabaseConfig, dao::{items::transfers, trades::TradeItem}, db::outbox::Outbox, sde::Sde};

async fn setup() -> (Db, Arc<World>, i32) {
    let db = Db::connect(&DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ty = sde.types.iter().next().expect("SDE type required").type_id;
    let world = World::new(db.clone(), sde, 1);
    (db, world, ty)
}

async fn job(db: &Db, trade_id: i64) -> Option<Job> {
    let row = db.query_opt("SELECT job_id, kind, due_at, payload FROM game.jobs WHERE job_id=$1", &[&id(trade_id)]).await.unwrap()?;
    Some(Job { id: row.get("job_id"), kind: row.get("kind"), due_at: row.get("due_at"), payload: row.get("payload") })
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL and serial execution"]
async fn stale_claim_expiry_and_idempotency() {
    let (db, world, ty) = setup().await;
    let old = client::filetime_now() - client::filetime_ticks_from_seconds(crate::config::game().trade.idle_timeout_secs + 1);
    let mut trade = trades::create(&db, 1, 2, 3, old).await.unwrap();
    let root = transfers::insert(&db, ty, 2, trade.id, 7, 1).await.unwrap();
    trades::add_item(&db, trade.id, &TradeItem { id: root, owner: 2, origin: 123, flag: 9 }).await.unwrap();
    assert!(refresh(&db, &mut Outbox::default(), &trade).await.is_ok());
    let captured = job(&db, trade.id).await.unwrap();
    trade.updated_at = client::filetime_now();
    assert!(trades::update(&db, &trade, trade.updated_at).await.unwrap());
    assert!(refresh(&db, &mut Outbox::default(), &trade).await.is_ok());
    assert!(handle(db.clone(), world.clone(), captured.clone()).await.is_ok());
    assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "open");
    assert_eq!(job(&db, trade.id).await.unwrap().due_at, deadline(trade.updated_at).unwrap());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(2, sender);
    trade.updated_at = old;
    assert!(trades::update(&db, &trade, old).await.unwrap());
    assert!(handle(db.clone(), world.clone(), captured.clone()).await.is_ok());
    assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "cancelled");
    assert!(job(&db, trade.id).await.is_none());
    let row = db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), 123);
    assert_eq!(row.get::<_, i32>("flag_id"), 9);
    assert_eq!(std::iter::from_fn(|| receiver.try_recv().ok()).filter(|(event, _)| event == "OnTradeCancel").count(), 1);
    assert!(handle(db.clone(), world.clone(), captured.clone()).await.is_ok());
    assert!(receiver.try_recv().is_err());
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade.id]).await.unwrap();
    assert!(handle(db.clone(), world, captured).await.is_ok());
}

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL and serial execution"]
async fn rollback_discards_domain_job_and_effects() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (db, _world, _) = setup().await;
    let trade = trades::create(&db, 1, 2, 3, client::filetime_now()).await.unwrap();
    assert!(refresh(&db, &mut Outbox::default(), &trade).await.is_ok());
    let original = job(&db, trade.id).await.unwrap();
    let dispatched = AtomicBool::new(false);
    let result: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox<TradeEffect>| {
        let mut changed = trade.clone();
        Box::pin(async move {
            changed.updated_at += 1;
            changed.money_a = 5.0;
            assert!(trades::update(tx, &changed, changed.updated_at).await.map_err(db_err)?);
            refresh(tx, outbox, &changed).await?;
            Err(CallError::Internal(anyhow::anyhow!("rollback")))
        })
    }, |(), _| async { dispatched.store(true, Ordering::SeqCst); Ok(()) }).await;
    assert!(result.is_err());
    assert!(!dispatched.load(Ordering::SeqCst));
    let stored = trades::get(&db, trade.id).await.unwrap().unwrap();
    assert_eq!(stored.updated_at, trade.updated_at);
    assert_eq!(stored.money_a, trade.money_a);
    assert_eq!(job(&db, trade.id).await.unwrap().due_at, original.due_at);
    assert!(cancel(&db, trade.id).await.is_ok());
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade.id]).await.unwrap();
}

#[tokio::test]
#[ignore = "requires a job-free dedicated INFINITY_TEST_DATABASE_URL and serial execution"]
async fn real_runner_expires_trade_and_shuts_down() {
    let (db, world, ty) = setup().await;
    crate::dao::jobs::tests::require_job_free_database(&db).await;
    let old = client::filetime_now() - client::filetime_ticks_from_seconds(crate::config::game().trade.idle_timeout_secs + 1);
    let trade = trades::create(&db, 1, 2, 3, old).await.unwrap();
    let root = transfers::insert(&db, ty, 2, trade.id, 7, 1).await.unwrap();
    trades::add_item(&db, trade.id, &TradeItem { id: root, owner: 2, origin: 123, flag: 9 }).await.unwrap();
    assert!(refresh(&db, &mut Outbox::default(), &trade).await.is_ok());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(2, sender);
    let (shutdown, watch) = tokio::sync::watch::channel(false);
    let runner = scheduler::runner::start(db.clone(), world, vec![registration()], crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some((event, _)) = receiver.recv().await { if event == "OnTradeCancel" { return; } }
        panic!("pilot channel closed before cancellation");
    }).await.expect("runner did not expire trade");
    shutdown.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), runner).await.unwrap().unwrap();
    assert_eq!(trades::get(&db, trade.id).await.unwrap().unwrap().state, "cancelled");
    assert!(job(&db, trade.id).await.is_none());
    let row = db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
    assert_eq!(row.get::<_, i64>("location_id"), 123);
    assert_eq!(row.get::<_, i32>("flag_id"), 9);
    db.execute("DELETE FROM game.items WHERE item_id=$1", &[&root]).await.unwrap();
    db.execute("DELETE FROM game.trades WHERE trade_id=$1", &[&trade.id]).await.unwrap();
}
