//! Startup recovery jobs reconcile overdue training and existing viewer snapshots.
use std::sync::Arc;
use crate::{dao::skills::{plan_milestones, plans, queue, training}, scheduler::Job, services::tests::fixture, space::World};

#[tokio::test]
#[ignore = "requires an isolated dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn startup_seed_pages_existing_viewers_and_handler_settles_overdue_training() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    assert!(crate::dao::skills::plan_progress::characters_after(&db, 0).await.unwrap().is_empty(), "dedicated database must have no existing snapshots");
    let owner = fixture::create(&db).await.unwrap();
    let absent = fixture::create(&db).await.unwrap();
    let skill = sde.types.iter().find(|t| super::super::validation::valid_skill(&sde, t.type_id) && sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let points = crate::services::skills::formulas::sp_for_level(crate::services::skills::training::skill_type(&sde, skill).rank, 1);
    let plan = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    let milestone = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    plans::create(&db, &plan, owner.character, "plan", "", &[skill], &[1]).await.unwrap();
    plan_milestones::create(&db, &milestone, &plan, "skill", skill, Some(1)).await.unwrap();
    training::starting_skill(&db, owner.character, skill, 0, 0).await.unwrap();
    queue::insert(&db, owner.character, &queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: points, start_time: Some(1), end_time: Some(2) }).await.unwrap();
    let mut viewers = vec![owner];
    for _ in 0..100 { viewers.push(fixture::create(&db).await.unwrap()); }
    for viewer in &viewers {
        crate::dao::skills::plan_progress::upsert(&db, viewer.character, &[crate::dao::skills::plan_progress::Snapshot { milestone_id: milestone.clone(), required_sp: points, trained_sp: 0, queued_sp: 0, completed: false, valid: true }], 1).await.unwrap();
    }
    super::seed(&db).await.unwrap();
    let before = db.query("SELECT job_id,due_at,created_at,updated_at FROM game.jobs WHERE kind=$1 ORDER BY job_id", &[&super::KIND]).await.unwrap();
    assert_eq!(before.len(), viewers.len());
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", super::KIND, absent.character)]).await.unwrap().is_none());
    let jobs: Vec<_> = before.iter().map(|r| (r.get::<_, String>("job_id"), r.get::<_, i64>("due_at"), r.get::<_, i64>("created_at"), r.get::<_, i64>("updated_at"))).collect();
    super::seed(&db).await.unwrap();
    let after: Vec<_> = db.query("SELECT job_id,due_at,created_at,updated_at FROM game.jobs WHERE kind=$1 ORDER BY job_id", &[&super::KIND]).await.unwrap().iter().map(|r| (r.get::<_, String>("job_id"), r.get::<_, i64>("due_at"), r.get::<_, i64>("created_at"), r.get::<_, i64>("updated_at"))).collect();
    assert_eq!(jobs, after);
    let world = World::new(db.clone(), sde, 1);
    let job = Job { id: format!("{}:{}", super::KIND, owner.character), kind: super::KIND.into(), due_at: 1, payload: owner.character.to_string() };
    super::handle(db.clone(), world.clone(), job.clone()).await.unwrap();
    let trained = training::current(&db, owner.character).await.unwrap();
    assert!(trained.iter().any(|s| s.type_id == skill && s.level == 1 && s.skill_points == points));
    assert!(queue::list(&db, owner.character).await.unwrap().is_empty());
    let row = db.query_one("SELECT completed,trained_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(row.get::<_, bool>("completed"));
    assert_eq!(row.get::<_, i64>("trained_sp"), points);
    let events = crate::db::outbox::tests::events(&db, "SkillTrained", owner.character).await;
    assert_eq!(events.len(), 1);
    super::handle(db.clone(), world, job).await.unwrap();
    assert_eq!(crate::db::outbox::tests::events(&db, "SkillTrained", owner.character).await.len(), 1);
    for viewer in &viewers { db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", super::KIND, viewer.character)]).await.unwrap(); }
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", crate::services::skills::completion::KIND, owner.character)]).await.unwrap();
    plans::delete(&db, owner.character, &plan).await.unwrap();
    for viewer in viewers { fixture::cleanup(&db, viewer).await.unwrap(); }
    fixture::cleanup(&db, absent).await.unwrap();
}
