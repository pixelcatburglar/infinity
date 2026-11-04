//! Durable queue completion and replay regressions.
use super::*;
mod reconciliation;
mod expert_expiry;
use crate::services::tests::{client, fixture};

async fn state(ex: &(impl crate::db::executor::Executor + ?Sized), character: i64) -> String {
    let job = format!("{KIND}:{character}");
    ex.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY a.item_id,a.attribute_id), '') FROM game.item_attributes a JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY job_id), '') FROM game.jobs j WHERE job_id=$2) AS fingerprint", &[&character, &job]).await.unwrap().get("fingerprint")
}

async fn apply(db: &Db, sde: &Arc<Sde>, character: i64, through: i64) -> Vec<crate::db::outbox::Effect<crate::services::effects::online::OnlineChanged>> {
    let _market = crate::services::market::lock().await;
    db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
        let sde = sde.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, character).await.map_err(db_err)?;
            let progress = advance_tx(tx, outbox, &sde, character, through).await?;
            finish_tx(tx, outbox, &sde, character, through, &progress, &[]).await?;
            Ok(())
        })
    }, |(), outbox| async move { Ok::<_, CallError>({ let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries }) }).await.unwrap_or_else(|_| panic!("advancing timeline"))
}

fn notices(entries: &[crate::db::outbox::Effect<crate::services::effects::online::OnlineChanged>], name: &str) -> usize {
    use crate::db::outbox::{Effect, Notification};
    entries.iter().filter(|entry| matches!(entry, Effect::Notification(Notification::Pilot { event, .. }) if event == name)).count()
}

async fn booster(db: &Db, character: i64, type_id: i32, expiry: i64) -> i64 {
    db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,ARRAY[]::integer[]) RETURNING item_id", &[&type_id, &character, &crate::consts::FLAG_BOOSTER, &expiry]).await.unwrap().get("item_id")
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn completion_emits_one_lua_event_per_new_level_and_none_on_replay() {
    use crate::db::outbox::Effect;
    use crate::lua::events::GameEvent;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,1,1,2,100,200,1,2)", &[&f.character]).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let sde = c.sde.clone();
    let _market = crate::services::market::lock().await;
    let events = db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        Box::pin(async move {
        crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
        let plan = timeline::plan_tx(tx, &sde, f.character, filetime_now()).await?;
        let first = apply_plan_tx(tx, outbox, &sde, &plan).await?;
        assert_eq!(first.completed, vec![(1, 1), (1, 2)]);
        let before = state(tx, f.character).await;
        let mut replay: Outbox = Outbox::default();
        assert!(apply_plan_tx(tx, &mut replay, &sde, &plan).await.is_err());
        assert!(replay.into_entries().is_empty());
        assert_eq!(state(tx, f.character).await, before);
        Ok(())
    }) }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("failed to complete and replay skill queue"));
    let levels: Vec<_> = events.into_iter().filter_map(|entry| match entry {
        Effect::Event(GameEvent::SkillTrained { char_id, type_id, level }) if char_id == f.character && type_id == 1 => Some(level),
        _ => None,
    }).collect();
    assert_eq!(levels, vec![1, 2]);
    let history = crate::dao::skills::history::list(&db, f.character, 50).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(history.iter().all(|row| row.event_type_id == super::super::history::TRAINING_COMPLETE));
    assert_eq!(history.iter().map(|row| row.absolute_points).collect::<Vec<_>>(), vec![200, 100]);
    fixture::cleanup(&db, f).await.unwrap();
}
#[tokio::test]

#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn restart_catches_up_adjacent_levels_and_reschedules_then_cancels() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let future = filetime_now() + crate::consts::FILETIME_MINUTE;
    db.execute("INSERT INTO game.skill_queue (character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,1,1,2,100,200,1,2),($1,2,1,3,200,300,2,$2)", &[&f.character, &future]).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    seed(&db).await.unwrap_or_else(|_| panic!("failed to seed skill completion jobs after restart"));
    let id = format!("{KIND}:{}", f.character);
    let job = Job { id: id.clone(), kind: KIND.into(), due_at: 1, payload: f.character.to_string() };
    handle(db.clone(), c.world.clone(), job.clone()).await.unwrap();
    let skills = training::current(&db, f.character).await.unwrap();
    assert_eq!((skills[0].level, skills[0].skill_points), (2, 200));
    let rows = queue::list(&db, f.character).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].position, rows[0].to_level), (0, 3));
    let scheduled = db.query_one("SELECT due_at FROM game.jobs WHERE job_id = $1", &[&id]).await.unwrap();
    assert_eq!(scheduled.get::<_, i64>("due_at"), future);
    let before = fixture::skill_fingerprint(&db, f.character).await.unwrap();
    handle(db.clone(), c.world.clone(), job.clone()).await.unwrap();
    assert_eq!(fixture::skill_fingerprint(&db, f.character).await.unwrap(), before);
    db.execute("UPDATE game.skill_queue SET end_time = 1 WHERE character_id = $1", &[&f.character]).await.unwrap();
    handle(db.clone(), c.world.clone(), job).await.unwrap();
    assert!(queue::list(&db, f.character).await.unwrap().is_empty());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&id]).await.unwrap().is_none());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires an empty dedicated INFINITY_TEST_DATABASE_URL with SDE and serial execution"]
async fn real_runner_delivers_due_completion_to_online_pilot_channel() {
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};

    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(crate::dao::jobs::next_due(&db, &[]).await.unwrap().is_none());
    let f = fixture::create(&db).await.unwrap();
    fixture::seed_skill(&db, f.character).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    seed(&db).await.unwrap_or_else(|_| panic!("failed to seed skill completion job for runner"));
    let id = format!("{KIND}:{}", f.character);
    let start = filetime_now();
    let due = start + crate::client::filetime_ticks_from_seconds(5);
    let scheduled = db.query_one("WITH updated_queue AS (UPDATE game.skill_queue SET start_time = $2, end_time = $3 WHERE character_id = $1 RETURNING end_time) UPDATE game.jobs SET due_at = updated_queue.end_time FROM updated_queue WHERE job_id = $4 RETURNING game.jobs.due_at", &[&f.character, &start, &due, &id]).await.unwrap();
    let due: i64 = scheduled.get("due_at");
    let (shutdown, watch) = watch::channel(false);
    let mut runner = scheduler::runner::start(
        db.clone(), c.world.clone(), vec![scheduler::register(KIND, handle)],
        crate::config::scheduler::SchedulerConfig::default(), watch,
    ).unwrap();
    scheduler::wake();

    // A separate task lets the runner drain and fixture cleanup run after assertion failures.
    let observed = tokio::spawn(async move {
        assert!(filetime_now() + crate::client::filetime_ticks_from_seconds(1) < due);
        assert!(tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await.is_err(),
            "pilot received a notification before the deadline");
        let mut events = Vec::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            while events.len() < 3 {
                let (event, args) = receiver.recv().await.expect("pilot channel closed");
                assert!(filetime_now() >= due, "notification arrived before the deadline");
                if event == "OnServerSkillsChanged" {
                    assert!(matches!(args.get(2), Some(Value::Int(now)) if *now >= due));
                }
                events.push(event);
            }
        }).await.expect("runner did not deliver completion notifications");
        (events, receiver)
    }).await;
    let _ = shutdown.send(true);
    let drained = tokio::time::timeout(Duration::from_secs(10), &mut runner).await;
    if drained.is_err() {
        runner.abort();
        let _ = runner.await;
    }
    let checked = tokio::spawn({
        let db = db.clone();
        let id = id.clone();
        async move {
            drained.expect("runner did not drain").expect("runner panicked");
            let (mut events, mut receiver) = observed.expect("notification assertions failed");
            while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
            assert_eq!(events.iter().filter(|event| event.as_str() == "OnServerSkillsChanged").count(), 1);
            assert_eq!(events.iter().filter(|event| event.as_str() == BRAIN_REFRESH).count(), 1);
            assert_eq!(events.iter().filter(|event| event.as_str() == "OnNewSkillQueueSaved").count(), 1);
            let skills = training::current(&db, f.character).await.unwrap();
            assert_eq!(skills.len(), 1);
            assert_eq!((skills[0].level, skills[0].skill_points), (1, 100));
            assert!(queue::list(&db, f.character).await.unwrap().is_empty());
            assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id = $1", &[&id]).await.unwrap().is_none());
        }
    }).await;
    scheduler::cancel(&db, &id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    checked.expect("runner completion assertions failed");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn harmless_expiries_preserve_queue_and_notify_once() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let skill_type = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()).expect("test SDE has no skill").type_id;
    training::starting_skill(&db, f.character, skill_type, 0, 0).await.unwrap();
    let mut harmless = None;
    for data in c.sde.types.iter().filter(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some()) {
        let first = booster(&db, f.character, data.type_id, 100).await;
        let second = booster(&db, f.character, data.type_id, 200).await;
        let before = super::super::attributes::character_attributes_at(&db, &c.sde, f.character, 99).await.unwrap_or_else(|_| panic!("loading active boosters"));
        let middle = super::super::attributes::character_attributes_at(&db, &c.sde, f.character, 100).await.unwrap_or_else(|_| panic!("loading first expiry"));
        let after = super::super::attributes::character_attributes_at(&db, &c.sde, f.character, 200).await.unwrap_or_else(|_| panic!("loading second expiry"));
        if before == middle && middle == after {
            harmless = Some((first, second));
            break;
        }
        for item in [first, second] { db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap(); }
    }
    let (first, second) = harmless.expect("test SDE has no booster harmless to training attributes");
    let row = queue::QueueEntry { position: 0, type_id: skill_type, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(500) };
    queue::insert(&db, f.character, &row).await.unwrap();
    let events = apply(&db, &c.sde, f.character, 250).await;
    assert!(queue::list(&db, f.character).await.unwrap() == vec![row]);
    assert!(brain::get(&db, f.character, first).await.unwrap().is_none());
    assert!(brain::get(&db, f.character, second).await.unwrap().is_none());
    assert_eq!(notices(&events, "OnItemChange"), 2);
    assert_eq!(notices(&events, BRAIN_REFRESH), 1);
    assert_eq!(notices(&events, "OnServerBoostersChanged"), 1);
    assert_eq!(notices(&events, "OnNewSkillQueueSaved"), 0);
    assert_eq!(notices(&events, "OnServerSkillsChanged"), 0);
    let replay = apply(&db, &c.sde, f.character, 250).await;
    assert!(replay.is_empty());
    scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn live_deadlines_match_delayed_application() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let live = fixture::create(&db).await.unwrap();
    let late = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), live.character).await;
    let booster_type = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some()).expect("test SDE has no booster").type_id;
    let skill_type = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()).expect("test SDE has no skill").type_id;
    for character in [live.character, late.character] {
        training::starting_skill(&db, character, skill_type, 0, 0).await.unwrap();
        queue::insert(&db, character, &queue::QueueEntry { position: 0, type_id: skill_type, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(200) }).await.unwrap();
        queue::insert(&db, character, &queue::QueueEntry { position: 1, type_id: skill_type, to_level: 2, start_sp: 100, dest_sp: 200, start_time: Some(200), end_time: Some(400) }).await.unwrap();
        for expiry in [100, 200, 200, 300] { booster(&db, character, booster_type, expiry).await; }
    }
    for at in [100, 200, 300, 350] { apply(&db, &c.sde, live.character, at).await; }
    apply(&db, &c.sde, late.character, 350).await;
    assert!(training::current(&db, live.character).await.unwrap() == training::current(&db, late.character).await.unwrap());
    assert!(queue::list(&db, live.character).await.unwrap() == queue::list(&db, late.character).await.unwrap());
    let left = crate::dao::skills::history::list(&db, live.character, 50).await.unwrap();
    let right = crate::dao::skills::history::list(&db, late.character, 50).await.unwrap();
    assert_eq!(left.len(), right.len());
    for (a, b) in left.iter().zip(&right) {
        assert_eq!((a.skill_type_id, a.event_type_id, a.absolute_points, a.log_date), (b.skill_type_id, b.event_type_id, b.absolute_points, b.log_date));
    }
    assert!(brain::list(&db, live.character).await.unwrap().is_empty());
    assert!(brain::list(&db, late.character).await.unwrap().is_empty());
    for f in [live, late] {
        scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
        fixture::cleanup(&db, f).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn manual_attribute_changes_retime_only_when_effective_values_change() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let skill = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()).expect("test SDE has no skill").type_id;
    training::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    let row = queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 1000, start_time: Some(100), end_time: Some(1100) };
    queue::insert(&db, f.character, &row).await.unwrap();
    let _market = crate::services::market::lock().await;
    db.transaction(|tx, _: &mut Outbox| {
        let sde = c.sde.clone();
        let row = row.clone();
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
            let before = super::super::attributes::character_attributes_at(tx, &sde, f.character, 600).await?;
            let mut progress = AdvanceResult::default();
            super::super::training::retime_after_change_tx(tx, &sde, f.character, 600, &before, &mut progress).await?;
            assert!(!progress.queue_changed);
            assert!(queue::list(tx, f.character).await.map_err(db_err)? == vec![row]);
            let state = crate::dao::characters::attributes::initialize_respec(tx, f.character, 0).await.map_err(db_err)?;
            let bases: Vec<i32> = crate::consts::dogma::CHAR_ATTRIBUTES.iter().map(|id| before[id] as i32 + 10).collect();
            assert!(crate::dao::characters::attributes::save_respec(tx, f.character, &bases, &state, 0, 600, 600).await.map_err(db_err)?);
            super::super::training::retime_after_change_tx(tx, &sde, f.character, 600, &before, &mut progress).await?;
            assert!(progress.queue_changed);
            let rows = queue::list(tx, f.character).await.map_err(db_err)?;
            assert_eq!(rows[0].start_sp, 500);
            assert_eq!(rows[0].start_time, Some(600));
            assert!(rows[0].end_time.unwrap() > 600);
            assert_eq!(training::current(tx, f.character).await.map_err(db_err)?[0].skill_points, 500);
            Ok(())
        })
    }, |(), _| async move { Ok::<_, CallError>(()) }).await.unwrap_or_else(|_| panic!("checking unchanged attributes"));
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn database_failure_after_completion_rolls_back_skill_queue_and_item_changes() {
    use std::sync::atomic::{AtomicBool, Ordering};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let booster_type = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some()).expect("test SDE has no booster").type_id;
    let skill = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()).expect("test SDE has no skill").type_id;
    training::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    queue::insert(&db, f.character, &queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(200) }).await.unwrap();
    let item = booster(&db, f.character, booster_type, 250).await;
    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,$3)", &[&item, &crate::consts::dogma::ATTR_BOOSTERNESS, &1.0f64]).await.unwrap();
    reschedule(&db, f.character).await.unwrap_or_else(|_| panic!("scheduling fixture"));
    let before = state(&db, f.character).await;
    let dispatched = Arc::new(AtomicBool::new(false));
    let result = {
        let _market = crate::services::market::lock().await;
        db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = c.sde.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
                let progress = advance_tx(tx, outbox, &sde, f.character, 300).await?;
                finish_tx(tx, outbox, &sde, f.character, 300, &progress, &[]).await?;
                assert_eq!(progress.completed, vec![(skill, 1)]);
                assert!(brain::get(tx, f.character, item).await.map_err(db_err)?.is_none());
                tx.query_one("SELECT 1 / 0", &[]).await.map_err(|error| db_err(crate::db::Error::from(error)))?;
                Ok(())
            })
        }, |(), _| {
            let dispatched = dispatched.clone();
            async move { dispatched.store(true, Ordering::SeqCst); Ok::<_, CallError>(()) }
        }).await
    };
    let unchanged = state(&db, f.character).await == before;
    scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    match result {
        Err(CallError::Internal(error)) => match error.downcast_ref::<crate::db::Error>() {
            Some(crate::db::Error::Postgres(error)) => assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)),
            _ => panic!("expected a PostgreSQL division-by-zero error"),
        },
        _ => panic!("expected a PostgreSQL division-by-zero error"),
    }
    assert!(!dispatched.load(Ordering::SeqCst));
    assert!(unchanged);
}

#[tokio::test]
#[ignore = "requires an empty dedicated INFINITY_TEST_DATABASE_URL with SDE and serial execution"]
async fn runner_delivers_idle_booster_expiry_after_restart_seed() {
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    assert!(crate::dao::jobs::next_due(&db, &[]).await.unwrap().is_none());
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let booster_type = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some()).expect("test SDE has no booster").type_id;
    let due = filetime_now() + crate::client::filetime_ticks_from_seconds(5);
    let item = booster(&db, f.character, booster_type, due).await;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    c.world.register_pilot(f.character, sender);
    seed(&db).await.unwrap_or_else(|_| panic!("seeding idle booster job"));
    let id = format!("{KIND}:{}", f.character);
    let job = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap();
    assert_eq!(job.get::<_, i64>("due_at"), due);
    let (shutdown, watch) = watch::channel(false);
    let mut runner = scheduler::runner::start(db.clone(), c.world.clone(), vec![scheduler::register(KIND, handle)], crate::config::scheduler::SchedulerConfig::default(), watch).unwrap();
    scheduler::wake();
    let observed = tokio::spawn(async move {
        assert!(tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await.is_err());
        let mut events = Vec::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            while events.len() < 3 {
                let (event, _) = receiver.recv().await.expect("pilot channel closed");
                assert!(filetime_now() >= due);
                events.push(event);
            }
        }).await.expect("idle booster notifications missing");
        (events, receiver)
    }).await;
    let _ = shutdown.send(true);
    let drained = tokio::time::timeout(Duration::from_secs(10), &mut runner).await;
    if drained.is_err() { runner.abort(); let _ = runner.await; }
    let checked = tokio::spawn({
        let db = db.clone();
        let id = id.clone();
        async move {
            drained.expect("runner did not drain").expect("runner panicked");
            let (mut events, mut receiver) = observed.expect("notification assertions failed");
            while let Ok((event, _)) = receiver.try_recv() { events.push(event); }
            assert_eq!(events.iter().filter(|e| e.as_str() == "OnItemChange").count(), 1);
            assert_eq!(events.iter().filter(|e| e.as_str() == BRAIN_REFRESH).count(), 1);
            assert_eq!(events.iter().filter(|e| e.as_str() == "OnServerBoostersChanged").count(), 1);
            assert_eq!(events.iter().filter(|e| e.as_str() == "OnNewSkillQueueSaved").count(), 0);
            assert!(brain::get(&db, f.character, item).await.unwrap().is_none());
            assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap().is_none());
        }
    }).await;
    handle(db.clone(), c.world.clone(), Job { id: id.clone(), kind: KIND.into(), due_at: due, payload: f.character.to_string() }).await.unwrap();
    assert!(receiver_replay_empty(&db, f.character).await);
    scheduler::cancel(&db, &id).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    checked.expect("idle expiry assertions failed");
}

async fn receiver_replay_empty(db: &Db, character: i64) -> bool {
    brain::list(db, character).await.unwrap().is_empty()
        && db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("{KIND}:{character}")]).await.unwrap().is_none()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn training_booster_expiry_banks_old_rate_and_matches_live_processing() {
    use crate::services::fit::{Fit, Key};
    use crate::consts::dogma::CHAR_ATTRIBUTES;
    use std::collections::HashMap;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let live = fixture::create(&db).await.unwrap();
    let late = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), live.character).await;
    let start = crate::consts::FILETIME_SECOND;
    let expiry = 10 * crate::consts::FILETIME_MINUTE;
    let mut exercised = false;
    'candidate: for data in c.sde.types.iter().filter(|t| t.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_some()) {
        for entry in data.effects() {
            let Some(effect) = c.sde.dogma.effect(entry.effect_id) else { continue };
            if effect.effect_category.unwrap_or(0) != 0 { continue; }
            for modifier in effect.modifiers() {
                if !CHAR_ATTRIBUTES.contains(&(modifier.modified as i64)) || modifier.modified == modifier.modifying { continue; }
                let mut items = Vec::new();
                for character in [live.character, late.character] {
                    let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,$5,ARRAY[$6::integer]) RETURNING item_id", &[&data.type_id, &character, &crate::consts::FLAG_BOOSTER, &start, &expiry, &effect.effect_id]).await.unwrap().get("item_id");
                    db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifier.modifying]).await.unwrap();
                    items.push(item);
                }
                let old_fit = Fit::load_character_at(&db, c.sde.clone(), live.character, expiry - 1).await.unwrap_or_else(|_| panic!("loading active booster"));
                let new_fit = Fit::load_character_at(&db, c.sde.clone(), live.character, expiry).await.unwrap_or_else(|_| panic!("loading expired booster"));
                let old: HashMap<_, _> = CHAR_ATTRIBUTES.iter().map(|&id| (id, old_fit.value(Key::Char, id as i32))).collect();
                let new: HashMap<_, _> = CHAR_ATTRIBUTES.iter().map(|&id| (id, new_fit.value(Key::Char, id as i32))).collect();
                if old != new {
                    for skill in c.sde.types.iter().filter(|s| s.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()) {
                        let mut row = queue::QueueEntry { position: 0, type_id: skill.type_id, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
                        let mut slower = row.clone();
                        let Ok(old_end) = super::super::training::schedule(&c.sde, &mut row, &old, start) else { continue };
                        let Ok(new_end) = super::super::training::schedule(&c.sde, &mut slower, &new, start) else { continue };
                        if old_end <= expiry || new_end <= old_end { continue; }
                        let banked = super::super::training::points_at(0, row.dest_sp, start, old_end, expiry);
                        if banked <= 0 || banked >= row.dest_sp { continue; }
                        for character in [live.character, late.character] {
                            training::starting_skill(&db, character, skill.type_id, 0, 0).await.unwrap();
                            queue::insert(&db, character, &row).await.unwrap();
                        }
                        apply(&db, &c.sde, live.character, expiry).await;
                        apply(&db, &c.sde, live.character, expiry + crate::consts::FILETIME_SECOND).await;
                        apply(&db, &c.sde, late.character, expiry + crate::consts::FILETIME_SECOND).await;
                        let left = queue::list(&db, live.character).await.unwrap();
                        let right = queue::list(&db, late.character).await.unwrap();
                        assert!(left == right);
                        assert_eq!(left[0].start_sp, banked);
                        assert_eq!(left[0].start_time, Some(expiry));
                        assert!(left[0].end_time.unwrap() > old_end);
                        assert!(training::current(&db, live.character).await.unwrap() == training::current(&db, late.character).await.unwrap());
                        assert_eq!(training::current(&db, late.character).await.unwrap()[0].skill_points, banked);
                        let final_end = left[0].end_time.unwrap();
                        apply(&db, &c.sde, live.character, old_end).await;
                        apply(&db, &c.sde, late.character, old_end).await;
                        assert!(!queue::list(&db, late.character).await.unwrap().is_empty());
                        assert!(crate::dao::skills::history::list(&db, late.character, 50).await.unwrap().is_empty());
                        apply(&db, &c.sde, live.character, final_end).await;
                        apply(&db, &c.sde, late.character, final_end).await;
                        assert!(queue::list(&db, late.character).await.unwrap().is_empty());
                        assert!(training::current(&db, live.character).await.unwrap() == training::current(&db, late.character).await.unwrap());
                        let a = crate::dao::skills::history::list(&db, live.character, 50).await.unwrap();
                        let b = crate::dao::skills::history::list(&db, late.character, 50).await.unwrap();
                        assert_eq!(a.len(), 1);
                        assert_eq!(b.len(), 1);
                        assert_eq!((a[0].absolute_points, a[0].log_date), (b[0].absolute_points, b[0].log_date));
                        assert_eq!(b[0].log_date, final_end);
                        exercised = true;
                        break 'candidate;
                    }
                }
                for item in items { db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap(); }
            }
        }
    }
    for f in [live, late] {
        scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
        fixture::cleanup(&db, f).await.unwrap();
    }
    assert!(exercised, "no training booster and compatible skill found in test SDE");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn committed_effects_follow_timeline() {
    use crate::db::outbox::{Effect, Notification};
    use crate::lua::events::GameEvent;
    use crate::consts::dogma::ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES;
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    for completion_at in [50_i64, 150, 100] {
        let f = fixture::create(&db).await.unwrap();
        let (c, _peer) = client(db.clone(), f.character).await;
        let credit_type = c.sde.types.iter().find(|t| t.attribute(ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES).is_some_and(|v| v >= 1.0 && v.fract() == 0.0)).expect("credit type in SDE").type_id;
        let skill_type = c.sde.types.iter().find(|t| t.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()).expect("skill in SDE").type_id;
        training::starting_skill(&db, f.character, skill_type, 0, 0).await.unwrap();
        queue::insert(&db, f.character, &queue::QueueEntry { position: 0, type_id: skill_type, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(completion_at) }).await.unwrap();
        let later = booster(&db, f.character, credit_type, 200).await;
        let early = booster(&db, f.character, credit_type, 100).await;
        let early_second = booster(&db, f.character, credit_type, 100).await;
        let remaining = booster(&db, f.character, credit_type, 300).await;
        for id in [later, early, early_second, remaining] {
            db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,1)", &[&id, &ATTR_NON_DIMINISHING_SKILL_INJECTOR_USES]).await.unwrap();
        }
        assert!(later < early && early < early_second);
        let _market = crate::services::market::lock().await;
        let sde = c.sde.clone();
        let (plan, effects) = db.transaction(|tx, outbox: &mut Outbox<crate::services::effects::online::OnlineChanged>| {
            let sde = sde.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
                let plan = timeline::plan_tx(tx, &sde, f.character, 250).await?;
                let progress = apply_plan_tx(tx, outbox, &sde, &plan).await?;
                finish_tx(tx, outbox, &sde, f.character, 250, &progress, &[]).await?;
                Ok(plan)
            })
        }, |plan, outbox| async move { Ok::<_, CallError>((plan, { let entries = outbox.into_entries(); assert!(!entries.iter().any(|effect| matches!(effect, crate::db::outbox::Effect::Operation(_)))); entries })) }).await.unwrap_or_else(|_| panic!("applying timeline"));
        let actual: Vec<String> = effects.iter().map(|effect| match effect {
            Effect::Event(GameEvent::SkillTrained { char_id, type_id, level }) if *char_id == f.character => {
                assert_eq!((*type_id, *level), (skill_type, 1));
                "trained".into()
            }
            Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnItemChange" => {
                let Value::DbRow { descriptor, .. } = &args[0] else { panic!("item notice is not a row") };
                let descriptor = marshal::DbRowDescriptor::from_value(descriptor).expect("item descriptor");
                let id = descriptor.get(&args[0], "itemID").and_then(|v| v.as_int()).expect("item ID");
                format!("item:{id}")
            }
            Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnNonDiminishingInjectionsRemoved" => {
                assert!(args.is_empty());
                "removed".into()
            }
            Effect::Notification(Notification::Pilot { char_id, event, args }) if *char_id == f.character && event == "OnNonDiminishingInjectionsAdded" => {
                format!("added:{}", args[0].as_int().expect("credit count"))
            }
            Effect::Notification(Notification::Pilot { char_id, event, .. }) if *char_id == f.character => event.clone(),
            _ => panic!("unexpected timeline effect"),
        }).collect();
        let mut expected = Vec::new();
        if completion_at <= 100 { expected.push("trained".to_string()); }
        expected.extend([format!("item:{early}"), format!("item:{early_second}"), "removed".into(), "added:2".into()]);
        if completion_at > 100 { expected.push("trained".into()); }
        expected.extend([format!("item:{later}"), "removed".into(), "added:1".into()]);
        expected.extend(["OnServerSkillsChanged".into(), BRAIN_REFRESH.into(), "OnNewSkillQueueSaved".into(), "OnServerBoostersChanged".into()]);
        assert_eq!(actual, expected);
        let history = crate::dao::skills::history::list(&db, f.character, 50).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!((history[0].skill_type_id, history[0].absolute_points, history[0].log_date), (skill_type, 100, completion_at));
        assert_eq!(super::super::trading::available_credits(&db, &c.sde, f.character, 250).await.unwrap_or_else(|_| panic!("reading remaining credits")), 1);
        for id in [later, early, early_second] {
            assert!(brain::get(&db, f.character, id).await.unwrap().is_none());
        }
        assert!(brain::get(&db, f.character, remaining).await.unwrap().is_some());
        assert!(queue::list(&db, f.character).await.unwrap().is_empty());
        let skills = training::current(&db, f.character).await.unwrap();
        assert_eq!((skills[0].level, skills[0].skill_points), (1, 100));
        let before = state(&db, f.character).await;
        let plan = Arc::new(plan);
        let replay = db.transaction(|tx, outbox: &mut Outbox| {
            let sde = sde.clone();
            let plan = plan.clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, f.character).await.map_err(db_err)?;
                assert!(apply_plan_tx(tx, outbox, &sde, &plan).await.is_err());
                Ok(())
            })
        }, |(), outbox| async move { Ok::<_, CallError>(outbox.into_entries()) }).await.unwrap_or_else(|_| panic!("replaying timeline"));
        assert!(replay.is_empty());
        assert_eq!(state(&db, f.character).await, before);
        scheduler::cancel(&db, &format!("{KIND}:{}", f.character)).await.unwrap();
        fixture::cleanup(&db, f).await.unwrap();
    }
}
