use super::super::*;
use super::fixtures::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn database_failure_after_install_rolls_back_stack_and_never_dispatches() {
    let _market = crate::services::market::lock().await;
    let (db, f, c, _peer) = setup().await;
    let ty = booster(&c.sde);
    let source = item(&db, f.character, ty, STATION, 2).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&source, &ATTR_BOOSTER_DURATION, &12345.0_f64]).await.unwrap();
    let now = filetime_now();
    let before = fingerprint(&db, f.character).await;
    let dispatched = Arc::new(AtomicBool::new(false));
    let flag = dispatched.clone();
    let draws: Vec<_> = candidates(&c.sde, ty).into_iter().map(|(id, _)| (id, 0.5)).collect();
    let world = crate::space::World::new(db.clone(), c.sde.clone(), 1);
    let fences = crate::services::effects::online::capture_online_fences(&world, &[f.character]).await;
    let result: Result<Value, CallError> = db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| {
        let sde = c.sde.clone(); let draws = draws.clone(); let fences = fences.clone();
        Box::pin(async move {
            let value = activate_tx(tx, outbox, &sde, &fences, f.character, None, source, Some(STATION), now, &draws, true).await?;
            let installed = brain::list(tx, f.character).await.map_err(db_err)?;
            assert_eq!(installed.len(), 1);
            assert_ne!(installed[0].item_id, source);
            assert!(installed[0].expires_at.is_some());
            assert_eq!(tx.query_one("SELECT quantity FROM game.items WHERE item_id=$1", &[&source]).await.map_err(|error| db_err(crate::db::Error::from(error)))?.get::<_, i64>("quantity"), 1);
            tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
            Ok(value)
        })
    }, |value, _outbox| async move { flag.store(true, Ordering::SeqCst); Ok(value) }).await;
    match result {
        Err(CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
            Some(crate::db::Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
    assert!(!dispatched.load(Ordering::SeqCst));
    assert_eq!(fingerprint(&db, f.character).await, before);
    cleanup(&db, f).await;
}
