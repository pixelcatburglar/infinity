//! Login preparation settles overdue training before reconciling milestone snapshots.
use std::sync::Arc;
use crate::{dao::{characters, skills::{plan_milestones, plans, queue, training}}, services::tests::fixture, space::World};

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn prepare_settles_overdue_training_and_updates_snapshot() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let owner = fixture::create(&db).await.unwrap();
    let station = sde.map.stations().next().unwrap().station_id;
    let system = sde.map.systems().find(|s| s.constellation_id.is_some() && s.region_id.is_some()).unwrap();
    db.execute("UPDATE game.characters SET station_id=$2,solar_system_id=$3,constellation_id=$4,region_id=$5 WHERE character_id=$1", &[&owner.character, &station, &system.solar_system_id, &system.constellation_id.unwrap(), &system.region_id.unwrap()]).await.unwrap();
    let skill = sde.types.iter().find(|t| crate::services::skill_plans::validation::valid_skill(&sde, t.type_id) && sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let points = crate::services::skills::formulas::sp_for_level(crate::services::skills::training::skill_type(&sde, skill).rank, 1);
    let plan = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    let milestone = [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat();
    plans::create(&db, &plan, owner.character, "plan", "", &[skill], &[1]).await.unwrap();
    plan_milestones::create(&db, &milestone, &plan, "skill", skill, Some(1)).await.unwrap();
    training::starting_skill(&db, owner.character, skill, 0, 0).await.unwrap();
    queue::insert(&db, owner.character, &queue::QueueEntry { position: 0, type_id: skill, to_level: 1, start_sp: 0, dest_sp: points, start_time: Some(1), end_time: Some(2) }).await.unwrap();
    crate::dao::skills::plan_progress::upsert(&db, owner.character, &[crate::dao::skills::plan_progress::Snapshot { milestone_id: milestone.clone(), required_sp: points, trained_sp: 0, queued_sp: 0, completed: false, valid: true }], 1).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let selected = characters::selection::selected(&db, owner.character, owner.account).await.unwrap().unwrap();
    super::super::prepare(&db, &world, &sde, owner.character, &selected, |n| n.dispatch(&world, None)).await.unwrap();
    let skills = training::current(&db, owner.character).await.unwrap();
    assert!(skills.iter().any(|s| s.type_id == skill && s.level == 1 && s.skill_points == points));
    assert!(queue::list(&db, owner.character).await.unwrap().is_empty());
    let row = db.query_one("SELECT completed,trained_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(row.get::<_, bool>("completed"));
    assert_eq!(row.get::<_, i64>("trained_sp"), points);
    plans::delete(&db, owner.character, &plan).await.unwrap();
    db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("{}:{}", crate::services::skills::completion::KIND, owner.character)]).await.unwrap();
    let owned = db.query("SELECT item_id FROM game.items WHERE owner_id=$1", &[&owner.character]).await.unwrap();
    for row in &owned { let id: i64 = row.get("item_id"); db.execute("DELETE FROM game.item_attributes WHERE item_id=$1", &[&id]).await.unwrap(); }
    db.execute("UPDATE game.characters SET ship_id=NULL WHERE character_id=$1", &[&owner.character]).await.unwrap();
    for row in &owned { let id: i64 = row.get("item_id"); db.execute("DELETE FROM game.items WHERE location_id=$1 AND owner_id=$2", &[&id, &owner.character]).await.unwrap(); }
    fixture::cleanup(&db, owner).await.unwrap();
}
