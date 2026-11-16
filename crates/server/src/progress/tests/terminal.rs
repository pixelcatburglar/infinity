//! Terminal receipts stop driving and keep their failure record.
use super::*;

fn poison<'a, 'tx>(tx: &'a Transaction<'tx>, _: &'a mut Outbox, _: &'a Sde, _: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> {
    Box::pin(async move {
        crate::dao::lua_store::set(tx, "terminal-test", "applied", "1").await?;
        Err(ApplyError::Permanent(CallError::Internal(anyhow::anyhow!("poison"))))
    })
}
struct Plan { key: DeliveryKey }
impl PreparedDelivery for Plan {
    fn key(&self) -> &DeliveryKey { &self.key }
    fn prelock<'a, 'tx>(&'a self, _: &'a Transaction<'tx>) -> TxFuture<'a, (), ApplyError> { Box::pin(async { Ok(()) }) }
    fn apply<'a, 'tx>(&'a self, tx: &'a Transaction<'tx>, outbox: &'a mut Outbox, sde: &'a Sde, event: &'a EventEnvelope) -> TxFuture<'a, (), ApplyError> { poison(tx, outbox, sde, event) }
}
fn prepare_terminal(_: ContentProgressResources, key: DeliveryKey) -> std::pin::Pin<Box<dyn Future<Output = Result<PrepareOutcome, ApplyError>> + Send>> {
    Box::pin(async move { Ok(PrepareOutcome::Ready(std::sync::Arc::new(Plan { key }))) })
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn terminal_receipts_stop_driving_and_keep_failure_record() {
    crate::tests::init_config();
    SEEN.store(0, Ordering::SeqCst);
    let config = crate::config::tests::test_database();
    let direct = DurableRegistration { consumer_key: "terminal-test", kinds: &["SkillTrained"], mode: DeliveryMode::Direct(poison) };
    let prepared = DurableRegistration { mode: DeliveryMode::Prepared(prepare_terminal), ..direct };
    let db = crate::db::Db::connect(&config).await.unwrap();
    // A prior failed run may leave its fixed consumer behind; remove it before
    // the fixture asserts an enabled, fully-declared subscription.
    for sql in [
        "DELETE FROM game.progress_deliveries WHERE consumer_key='terminal-test'",
        "DELETE FROM game.progress_subscription_kinds WHERE consumer_key='terminal-test'",
        "DELETE FROM game.progress_subscriptions WHERE consumer_key='terminal-test'",
    ] { db.execute(sql, &[]).await.unwrap(); }
    let registry = fixture_registry(&db, Registry::new(vec![observe], vec![direct]).unwrap()).await;
    let db = db.with_progress_registry(registry);
    let sde = std::sync::Arc::new(Sde::load(&db).await.unwrap());
    let event = GameEvent::SkillTrained { char_id: 1, type_id: 2, level: 3 };
    let entry = crate::dao::progress::JournalEvent { source_namespace: format!("terminal-test-{}", crate::handshake::random_u64()), source_key: "0".into(), schema_version: codec::SCHEMA_VERSION, kind: event.kind().into(), occurred_at: 1, recorded_at: 1, payload: codec::encode(&event).unwrap(), context: "{}".into(), origin: "test".into(), ordinal: 0, subject_character_id: Some(1) };
    let (consumers, kinds) = db.progress_registry().subscription_arrays();
    let key = DeliveryKey { consumer_key: direct.consumer_key.into(), event_id: crate::dao::progress::insert_batch(&db, &[entry], consumers, kinds).await.unwrap()[0].1.clone() };
    let world = crate::space::World::new(db.clone(), sde.clone(), 0);
    let apply = |registration: DurableRegistration, plan: Option<std::sync::Arc<dyn PreparedDelivery>>| {
        let (db, sde, key, world) = (db.clone(), sde.clone(), key.clone(), world.clone());
        async move {
            db.transaction::<_, _, ApplyError, Outbox, _, _, _>(|tx, outbox| {
                let (plan, key, sde) = (plan.clone(), key.clone(), sde.clone());
                Box::pin(async move { apply_once_tx(tx, outbox, &sde, &registration, key, plan.as_deref(), ClaimMode::Blocking).await })
            }, |_, outbox| async move { crate::db::outbox::dispatch_world((), outbox, &world).map_err(ApplyError::Call) }).await
        }
    };
    // A permanent error rolls the domain write, the queued event and the ack
    // back with its transaction; nothing publishes and the receipt stays pending.
    let plan: std::sync::Arc<dyn PreparedDelivery> = std::sync::Arc::new(Plan { key: key.clone() });
    for (registration, plan) in [(direct, None), (prepared, Some(plan.clone()))] {
        assert!(matches!(apply(registration, plan).await, Err(ApplyError::Permanent(_))));
        assert!(crate::dao::lua_store::get(&db, "terminal-test", "applied").await.unwrap().is_none());
    }
    assert_eq!(SEEN.load(Ordering::SeqCst), 0);
    assert!(matches!(crate::dao::progress::delivery(&db, &key).await.unwrap(), crate::dao::progress::DeliveryState::Pending(_)));
    let mut attempt = None;
    for round in 0..2 {
        attempt = Some(crate::dao::progress::due(&db, i64::MAX, 64, db.progress_registry().enabled_keys()).await.unwrap()
            .into_iter().find(|attempt| attempt.key == key).expect("pending receipt observation"));
        let update = crate::dao::progress::failed(&db, attempt.as_ref().unwrap(), crate::client::filetime_now(), "poison", true, 1).await.unwrap().unwrap();
        assert_eq!(update.terminal, round == 1);
    }
    assert!(matches!(crate::dao::progress::delivery(&db, &key).await.unwrap(), crate::dao::progress::DeliveryState::Terminal));
    assert!(matches!(apply(prepared, Some(plan)).await, Err(ApplyError::Terminal)));
    assert!(matches!(apply(direct, None).await, Err(ApplyError::Terminal)));
    assert!(crate::dao::progress::failed(&db, attempt.as_ref().unwrap(), crate::client::filetime_now(), "late", true, 1).await.unwrap().is_none());
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id=$1::text::uuid", &[&key.event_id]).await.unwrap();
    db.execute("DELETE FROM game.progress_events WHERE event_id=$1::text::uuid", &[&key.event_id]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscription_kinds WHERE consumer_key=$1", &[&key.consumer_key]).await.unwrap();
    db.execute("DELETE FROM game.progress_subscriptions WHERE consumer_key=$1", &[&key.consumer_key]).await.unwrap();
}
