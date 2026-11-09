use super::*;

fn row(id: i64, time: i64, corp: i64) -> ProgressLog {
    ProgressLog { log_id: id, character_id: 7, event_time: time, solar_system_id: 8, type_id: 9, quantity: 2, unit_volume_m3: 1.5, resource_kind: MiningResourceKind::Ore, corporation_id: Some(corp), alliance_id: None, faction_id: None, source_ship_id: 10, source_ship_type_id: 11 }
}

#[test]
fn grouping_preserves_exact_slices_and_historical_dimensions() {
    let groups = group(vec![row(1, 100, 1), row(2, 101, 1), row(3, 102, 2)]).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].slices.iter().map(|s| (s.log_id, s.occurred_at, s.quantity, s.volume_m3)).collect::<Vec<_>>(), vec![(1, 100, 2, 3.0), (2, 101, 2, 3.0)]);
    assert_eq!(groups[1].context.corporation_id, Some(2));
    assert!(group(vec![row(1, 1, 1), row(1, 2, 1)]).is_err());
    let mut bad = row(2, 2, 1); bad.unit_volume_m3 = f64::MAX;
    assert!(group(vec![bad]).is_err());
}

async fn insert(tx: &tokio_postgres::Transaction<'_>, character: i64, time: i64, corp: i64) -> Result<i64, CallError> {
    Ok(tx.query_one("INSERT INTO game.character_mining_ledger (character_id,event_time,solar_system_id,type_id,quantity,quantity_wasted,progress_aggregated,source_ship_id,source_ship_type_id,corporation_id,resource_kind,unit_volume_m3) VALUES ($1,$2,8,9,2,0,false,10,11,$3,'ore',1.5) RETURNING log_id", &[&character, &time, &corp]).await.map_err(db_err)?.get("log_id"))
}

async fn claim_only(db: &Db, id: &str, now: i64) -> anyhow::Result<crate::dao::jobs::Claim> {
    let ids: Vec<String> = crate::dao::jobs::tests::existing_ids(db).await?
        .into_iter().filter(|existing| existing != id).collect();
    let excluded: Vec<&str> = ids.iter().map(String::as_str).collect();
    let claim = crate::dao::jobs::claim(db, now, now + 100, 1, &excluded).await?
        .pop().ok_or_else(|| anyhow::anyhow!("test job was not claimable"))?;
    anyhow::ensure!(claim.job.id == id, "claimed an unrelated job");
    anyhow::ensure!(claim.job.kind == crate::scheduler::MINING_PROGRESS, "claimed job has incorrect kind");
    Ok(claim)
}

async fn cleanup_source(db: &Db, character: i64) -> anyhow::Result<()> {
    db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE subject_character_id=$1)", &[&character]).await?;
    db.execute("DELETE FROM game.progress_events WHERE subject_character_id=$1", &[&character]).await?;
    db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1", &[&character]).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL; run serially"]
async fn source_and_event_rollback_then_bounded_resume_is_idempotent() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = -crate::client::filetime_now();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let aborted: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        insert(tx, character, 100, 1).await?;
        aggregate_tx(tx, outbox, 1).await?;
        Err(anyhow::anyhow!("rollback").into())
    }), |value, _| async move { Ok(value) }).await;
    assert!(aborted.is_err());
    assert!(crate::dao::mining_ledger::list(&db, character, 0, 200).await.unwrap().is_empty());
    assert!(crate::db::outbox::tests::events(&db, "OreMined", character).await.is_empty());

    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        insert(tx, character, 100, 1).await?;
        insert(tx, character, 101, 1).await?;
        insert(tx, character, 102, 2).await?;
        Ok::<_, CallError>(())
    }), |value, _| async move { Ok(value) }).await.unwrap();
    drop(db);
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let rollback: Result<(), CallError> = db.transaction(|tx, outbox: &mut Outbox| Box::pin(async move {
        assert_eq!(aggregate_tx(tx, outbox, 2).await?, 2);
        Err(anyhow::anyhow!("rollback").into())
    }), |value, _| async move { Ok(value) }).await;
    assert!(rollback.is_err());
    assert!(crate::db::outbox::tests::events(&db, "OreMined", character).await.is_empty());
    for expected in [2, 1, 0] {
        let count = db.transaction(|tx, outbox: &mut Outbox| Box::pin(aggregate_tx(tx, outbox, 2)), |value, _| async move { Ok(value) }).await.unwrap();
        assert_eq!(count, expected);
    }
    let events = crate::db::outbox::tests::events(&db, "OreMined", character).await;
    assert_eq!(events.len(), 2);
    let mut slices = Vec::new();
    for event in events {
        if let GameEvent::OreMined { context, slices: part, .. } = event {
            for slice in part { slices.push((slice.occurred_at, slice.quantity, context.corporation_id)); }
        } else { panic!("unexpected event"); }
    }
    slices.sort();
    assert_eq!(slices, vec![(100, 2, Some(1)), (101, 2, Some(1)), (102, 2, Some(2))]);
    }).await;
    cleanup_source(&db, character).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL; run serially"]
async fn concurrent_aggregators_skip_locked_gap_and_journal_each_source_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = -crate::client::filetime_now();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let ids = db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        Ok::<_, CallError>(vec![insert(tx, character, 100, 1).await?, insert(tx, character, 101, 1).await?, insert(tx, character, 102, 1).await?])
    }), |value, _| async move { Ok(value) }).await.unwrap();
    let cfg = crate::config::tests::test_database().to_pg_config().unwrap();
    let (mut client, connection) = cfg.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let gap = client.transaction().await.unwrap();
    gap.query_one("SELECT log_id FROM game.character_mining_ledger WHERE log_id=$1 FOR UPDATE", &[&ids[1]]).await.unwrap();
    let run = || db.transaction(|tx, outbox: &mut Outbox| Box::pin(aggregate_tx(tx, outbox, 1)), |value, _| async move { Ok(value) });
    let (first, second) = tokio::join!(run(), run());
    assert_eq!((first.unwrap(), second.unwrap()), (1, 1));
    let rows = db.query("SELECT log_id, progress_aggregated FROM game.character_mining_ledger WHERE character_id=$1 ORDER BY log_id", &[&character]).await.unwrap();
    assert_eq!(rows.iter().map(|row| (row.get::<_, i64>("log_id"), row.get::<_, bool>("progress_aggregated"))).collect::<Vec<_>>(), vec![(ids[0], true), (ids[1], false), (ids[2], true)]);
    gap.commit().await.unwrap();
    assert_eq!(run().await.unwrap(), 1);
    assert_eq!(run().await.unwrap(), 0);
    let mut actual = Vec::new();
    for event in crate::db::outbox::tests::events(&db, "OreMined", character).await {
        if let GameEvent::OreMined { slices, .. } = event {
            actual.extend(slices.into_iter().map(|slice| slice.log_id));
        } else { panic!("unexpected event"); }
    }
    actual.sort_unstable();
    assert_eq!(actual, ids);
    let pending: i64 = db.query_one("SELECT count(*) AS count FROM game.character_mining_ledger WHERE character_id=$1 AND NOT progress_aggregated", &[&character]).await.unwrap().get("count");
    assert_eq!(pending, 0);
    }).await;
    cleanup_source(&db, character).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn seeded_periodic_job_reopens_and_retries_pending_source() {
    use crate::{dao::jobs, scheduler::{Job, MINING_PROGRESS}};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = -crate::client::filetime_now();
    let job_id = format!("test:mining-progress:{}", -character);
    crate::dao::jobs::tests::require_job_free_database(&db).await;
    assert!(!crate::dao::jobs::tests::existing_ids(&db).await.unwrap().contains(&job_id), "test job ID already exists");
    let seeded = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let test_seeded = seeded.clone();
    let test_db = db.clone();
    let test_job_id = job_id.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let job_id = test_job_id;
    let result: anyhow::Result<()> = async {
        let id = db.transaction(|tx, _: &mut Outbox| Box::pin(insert(tx, character, 100, 1)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        db.execute("UPDATE game.character_mining_ledger SET unit_volume_m3=NULL WHERE log_id=$1", &[&id]).await?;
        let now = crate::client::filetime_now();
        let job = Job { id: job_id.clone(), kind: MINING_PROGRESS.into(), due_at: now, payload: "{}".into() };
        anyhow::ensure!(jobs::seed(&db, &job, now).await? == 1, "job seed failed");
        test_seeded.store(true, std::sync::atomic::Ordering::SeqCst);
        let claim = claim_only(&db, &job_id, now).await?;
        let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await?), 0);
        anyhow::ensure!(handle(db.clone(), world, claim.job.clone()).await.is_err(), "invalid source succeeded");
        anyhow::ensure!(jobs::fail(&db, &claim.job.id, claim.token, now + 1, now, "invalid snapshot", false).await? == 1, "job failure not recorded");
        anyhow::ensure!(crate::db::outbox::tests::events(&db, "OreMined", character).await.is_empty(), "invalid source emitted event");
        db.execute("UPDATE game.character_mining_ledger SET unit_volume_m3=1.5 WHERE log_id=$1", &[&id]).await?;
        let reopened = Db::connect(&crate::config::tests::test_database()).await?;
        anyhow::ensure!(jobs::seed(&reopened, &job, now + 1).await? == 0, "scheduled job was reseeded");
        let claim = claim_only(&reopened, &job_id, now + 1).await?;
        anyhow::ensure!(claim.attempt == 2, "retry attempt was not retained");
        let world = World::new(reopened.clone(), Arc::new(crate::sde::Sde::load(&reopened).await?), 0);
        handle(reopened.clone(), world, claim.job.clone()).await?;
        anyhow::ensure!(jobs::complete_periodic(&reopened, &claim.job.id, claim.token, now + 200, now + 1).await? == 1, "job completion failed");
        let events = crate::db::outbox::tests::events(&reopened, "OreMined", character).await;
        anyhow::ensure!(events.len() == 1 && matches!(&events[0], GameEvent::OreMined { slices, .. } if slices.len() == 1 && slices[0].log_id == id), "unexpected events");
        anyhow::ensure!(reopened.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id=$1", &[&id]).await?.get::<_, bool>("progress_aggregated"), "source remains pending");
        Ok(())
    }.await;
    result
    }).await;
    let job_cleanup = if seeded.load(std::sync::atomic::Ordering::SeqCst) {
        jobs::cancel(&db, &job_id).await.map(|_| ())
    } else {
        Ok(())
    };
    let source_cleanup = cleanup_source(&db, character).await;
    job_cleanup.unwrap();
    source_cleanup.unwrap();
    result.unwrap().unwrap();
}

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL; run serially"]
async fn arithmetic_failure_keeps_source_rows_pending_without_journal() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = -crate::client::filetime_now();
    let test_db = db.clone();
    let test = tokio::spawn(async move {
    let db = test_db;
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move {
        insert(tx, character, 100, 1).await?;
        insert(tx, character, 101, 1).await?;
        tx.execute("UPDATE game.character_mining_ledger SET quantity=$2 WHERE character_id=$1", &[&character, &i64::MAX]).await.map_err(db_err)?;
        Ok::<_, CallError>(())
    }), |value, _| async move { Ok(value) }).await.unwrap();
    let result = db.transaction(|tx, outbox: &mut Outbox| Box::pin(aggregate_tx(tx, outbox, 2)), |value, _| async move { Ok(value) }).await;
    assert!(result.is_err());
    let pending: i64 = db.query_one("SELECT count(*) AS count FROM game.character_mining_ledger WHERE character_id=$1 AND NOT progress_aggregated", &[&character]).await.unwrap().get("count");
    assert_eq!(pending, 2);
    assert!(crate::db::outbox::tests::events(&db, "OreMined", character).await.is_empty());
    }).await;
    cleanup_source(&db, character).await.unwrap();
    test.unwrap();
}

#[tokio::test]
#[ignore = "requires an empty initialized dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn seeded_mining_job_retries_and_recovers_pending_after_reopen() {
    use crate::{client::filetime_now, dao::jobs, scheduler};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let character = -filetime_now();
    crate::dao::jobs::tests::require_job_free_database(&db).await;
    let existing = jobs::tests::existing_ids(&db).await.unwrap();
    for kind in scheduler::PERIODIC_KINDS {
        assert!(!existing.iter().any(|id| id == kind), "fixed periodic job ID already exists: {kind}; use a freshly initialized dedicated database");
    }
    let seed_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let test_seed_started = seed_started.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
    let db = test_db;
    let result: anyhow::Result<()> = async {
        let source = db.transaction(|tx, _: &mut Outbox| Box::pin(insert(tx, character, 100, 1)), |value, _| async move { Ok(value) }).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        db.execute("UPDATE game.character_mining_ledger SET unit_volume_m3=$2 WHERE log_id=$1", &[&source, &f64::MAX]).await?;
        test_seed_started.store(true, std::sync::atomic::Ordering::SeqCst);
        scheduler::seed(&db).await?;
        let registrations = crate::services::scheduler_registrations();
        let registration = registrations.iter().find(|r| r.kind == scheduler::MINING_PROGRESS).ok_or_else(|| anyhow::anyhow!("missing mining registration"))?;
        anyhow::ensure!(registration.interval_secs == Some(crate::config::game().mining.progress_aggregation_secs as u64), "incorrect interval");
        let now = filetime_now();
        let claim = claim_only(&db, scheduler::MINING_PROGRESS, now).await?;
        anyhow::ensure!(claim.job.kind == scheduler::MINING_PROGRESS, "incorrect job kind");
        let world = World::new(db.clone(), Arc::new(crate::sde::Sde::load(&db).await?), 0);
        anyhow::ensure!((registration.handler)(db.clone(), world, claim.job.clone()).await.is_err(), "invalid source succeeded");
        anyhow::ensure!(!db.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id=$1", &[&source]).await?.get::<_, bool>("progress_aggregated"), "invalid source aggregated");
        anyhow::ensure!(crate::db::outbox::tests::events(&db, "OreMined", character).await.is_empty(), "invalid source emitted event");
        anyhow::ensure!(jobs::fail(&db, &claim.job.id, claim.token, now, now, "invalid source", true).await? == 1, "job failure not recorded");
        db.execute("UPDATE game.character_mining_ledger SET unit_volume_m3=1.5 WHERE log_id=$1", &[&source]).await?;
        let reopened = Db::connect(&crate::config::tests::test_database()).await?;
        scheduler::seed(&reopened).await?;
        let now = filetime_now();
        let claim = claim_only(&reopened, scheduler::MINING_PROGRESS, now).await?;
        anyhow::ensure!(claim.attempt == 1, "dead job was not revived");
        let world = World::new(reopened.clone(), Arc::new(crate::sde::Sde::load(&reopened).await?), 0);
        (registration.handler)(reopened.clone(), world.clone(), claim.job.clone()).await?;
        let events = crate::db::outbox::tests::events(&reopened, "OreMined", character).await;
        anyhow::ensure!(events.len() == 1 && matches!(&events[0], GameEvent::OreMined { slices, .. } if slices.len() == 1 && (slices[0].log_id, slices[0].occurred_at, slices[0].quantity, slices[0].volume_m3) == (source, 100, 2, 3.0)), "unexpected event or slices");
        anyhow::ensure!(reopened.query_one("SELECT progress_aggregated FROM game.character_mining_ledger WHERE log_id=$1", &[&source]).await?.get::<_, bool>("progress_aggregated"), "source remains pending");
        anyhow::ensure!(jobs::complete_periodic(&reopened, &claim.job.id, claim.token, now, now).await? == 1, "job completion failed");
        let retry = claim_only(&reopened, scheduler::MINING_PROGRESS, now).await?;
        (registration.handler)(reopened.clone(), world, retry.job).await?;
        anyhow::ensure!(crate::db::outbox::tests::events(&reopened, "OreMined", character).await.len() == 1, "repeat emitted another event");
        Ok(())
    }.await;
    result
    }).await;
    let source_cleanup = cleanup_source(&db, character).await;
    let jobs_cleanup: anyhow::Result<()> = async {
        if seed_started.load(std::sync::atomic::Ordering::SeqCst) {
            let created = jobs::tests::existing_ids(&db).await?;
            for kind in scheduler::PERIODIC_KINDS {
                if created.iter().any(|id| id == kind) {
                    jobs::cancel(&db, kind).await?;
                }
            }
        }
        Ok(())
    }.await;
    source_cleanup.unwrap();
    jobs_cleanup.unwrap();
    result.unwrap().unwrap();
}
