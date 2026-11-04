use super::*;

async fn plan(db: &crate::db::Db, sde: &Arc<Sde>, char_id: i64, through: i64) -> AdvancePlan {
    db.transaction(|tx, _: &mut crate::db::outbox::Outbox| {
        let sde = Arc::clone(sde);
        Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            plan_tx(tx, &sde, char_id, through).await
        })
    }, |plan, outbox| async move {
        assert!(outbox.into_entries().is_empty());
        Ok(plan)
    }).await.unwrap_or_else(|_| panic!("planning timeline"))
}

async fn fingerprint(db: &crate::db::Db, char_id: i64) -> String {
    let job = format!("skills.complete:{char_id}");
    db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY type_id), '') FROM game.skills s WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(q)::text, '' ORDER BY position), '') FROM game.skill_queue q WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(i)::text, '' ORDER BY item_id), '') FROM game.items i WHERE owner_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(a)::text, '' ORDER BY a.item_id,a.attribute_id), '') FROM game.item_attributes a LEFT JOIN game.items i ON i.item_id=a.item_id WHERE i.owner_id=$1 OR a.item_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY history_id), '') FROM game.skill_history h WHERE character_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(j)::text, '' ORDER BY job_id), '') FROM game.jobs j WHERE job_id=$2) AS fingerprint", &[&char_id, &job]).await.unwrap().get("fingerprint")
}

#[test]
fn projected_points_include_only_clamped_active_progress() {
    let skill = Skill { type_id: 1, level: 1, skill_points: 100 };
    let row = QueueEntry { position: 0, type_id: 1, to_level: 2, start_sp: 100, dest_sp: 200, start_time: Some(10), end_time: Some(20) };
    let mut plan = AdvancePlan { char_id: 1, through: 15, initial_skills: vec![skill.clone()], initial_queue: vec![row.clone()], initial_brain: vec![], initial_installations: vec![], skills: vec![skill], queue: vec![row], events: vec![], queue_changed: false, brain_changed: false, boosters_changed: false };
    assert_eq!(plan.trained_points().unwrap_or_else(|_| panic!("projecting points")), 150);
    assert_eq!(plan.skills[0].skill_points, 100);
    plan.through = 30;
    assert_eq!(plan.trained_points().unwrap_or_else(|_| panic!("projecting capped points")), 200);
    plan.queue[0].end_time = None;
    assert_eq!(plan.trained_points().unwrap_or_else(|_| panic!("projecting inactive queue")), 100);
}

#[test]
fn interpolation_handles_extreme_timestamps() {
    assert_eq!(training::points_at(0, 100, i64::MIN, i64::MAX, i64::MIN), 0);
    assert_eq!(training::points_at(0, 100, i64::MIN, i64::MAX, i64::MAX), 100);
    assert_eq!(training::points_at(0, 100, 10, 20, 25), 100);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn planning_preserves_database_and_uses_same_fit() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let skills = crate::dao::skills::training::current(&db, f.character).await.unwrap();
    let persisted = Fit::load_character_at(&db, c.sde.clone(), f.character, 100).await.unwrap_or_else(|_| panic!("loading persisted fit"));
    let installations = crate::dao::skills::expert_systems::all(&db, f.character).await.unwrap();
    let projected = Fit::load_character_with_skills_at(&db, c.sde.clone(), f.character, 100, &skills, &installations).await.unwrap_or_else(|_| panic!("loading projected fit"));
    for attr in CHAR_ATTRIBUTES {
        assert_eq!(persisted.value(Key::Char, attr as i32), projected.value(Key::Char, attr as i32));
    }
    let queue = crate::dao::skills::queue::list(&db, f.character).await.unwrap();
    let brain = crate::dao::items::brain::list(&db, f.character).await.unwrap();
    let before = fingerprint(&db, f.character).await;
    let plan = plan(&db, &c.sde, f.character, 100).await;
    assert_eq!(plan.char_id, f.character);
    assert_eq!(fingerprint(&db, f.character).await, before);
    assert_eq!(plan.initial_skills.len(), skills.len());
    assert!(plan.queue == queue);
    assert_eq!(plan.initial_brain.len(), brain.len());
    let after_skills = crate::dao::skills::training::current(&db, f.character).await.unwrap();
    let after_queue = crate::dao::skills::queue::list(&db, f.character).await.unwrap();
    let after_brain = crate::dao::items::brain::list(&db, f.character).await.unwrap();
    assert!(after_skills == skills);
    assert!(after_queue == queue);
    assert_eq!(after_brain.len(), brain.len());
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn training_booster_expiry_banks_only_until_expiry() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let existing = crate::dao::skills::training::current(&db, f.character).await.unwrap();
    let baseline: i64 = existing.iter().map(|skill| skill.skill_points).sum();
    let mut exercised = false;
    'types: for data in c.sde.types.iter() {
        if data.attribute(crate::consts::dogma::ATTR_BOOSTERNESS).is_none() { continue; }
        for entry in data.effects() {
            let Some(effect) = c.sde.dogma.effect(entry.effect_id) else { continue };
            if effect.effect_category.unwrap_or(0) != 0 { continue; }
            for modifier in effect.modifiers() {
                if !CHAR_ATTRIBUTES.contains(&(modifier.modified as i64)) || modifier.modified == modifier.modifying { continue; }
                let expiry = crate::consts::FILETIME_MINUTE * 10;
                let start = crate::consts::FILETIME_SECOND;
                let item: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_activated_at,brain_expires_at,brain_effect_ids) VALUES ($1,$2,$2,$3,true,$4,$5,ARRAY[$6::integer]) RETURNING item_id", &[&data.type_id, &f.character, &FLAG_BOOSTER, &start, &expiry, &effect.effect_id]).await.unwrap().get("item_id");
                db.execute("INSERT INTO game.item_attributes (item_id,attribute_id,value) VALUES ($1,$2,10)", &[&item, &modifier.modifying]).await.unwrap();
                let before = Fit::load_character_at(&db, c.sde.clone(), f.character, expiry - 1).await.unwrap_or_else(|_| panic!("loading active booster"));
                let after = Fit::load_character_at(&db, c.sde.clone(), f.character, expiry).await.unwrap_or_else(|_| panic!("loading expired booster"));
                let old: HashMap<_, _> = CHAR_ATTRIBUTES.iter().map(|&id| (id, before.value(Key::Char, id as i32))).collect();
                let new: HashMap<_, _> = CHAR_ATTRIBUTES.iter().map(|&id| (id, after.value(Key::Char, id as i32))).collect();
                if old != new {
                    for skill in c.sde.types.iter().filter(|skill| skill.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && !existing.iter().any(|row| row.type_id == skill.type_id)) {
                        let mut old_row = QueueEntry { position: 0, type_id: skill.type_id, to_level: 1, start_sp: 0, dest_sp: 100_000, start_time: None, end_time: None };
                        let mut new_row = old_row.clone();
                        let Ok(old_end) = training::schedule(&c.sde, &mut old_row, &old, start) else { continue };
                        let Ok(new_end) = training::schedule(&c.sde, &mut new_row, &new, start) else { continue };
                        if old_end <= expiry || new_end <= old_end { continue; }
                        let banked = training::points_at(0, old_row.dest_sp, start, old_end, expiry);
                        if banked <= 0 || banked + 1 >= old_row.dest_sp { continue; }
                        crate::dao::skills::training::starting_skill(&db, f.character, skill.type_id, 0, 0).await.unwrap();
                        crate::dao::skills::queue::insert(&db, f.character, &old_row).await.unwrap();
                        let before = fingerprint(&db, f.character).await;
                        let projected = plan(&db, &c.sde, f.character, expiry + crate::consts::FILETIME_SECOND).await;
                        assert_eq!(fingerprint(&db, f.character).await, before);
                        assert!(projected.brain_changed && projected.boosters_changed && projected.queue_changed);
                        assert_eq!(projected.queue[0].start_sp, banked);
                        assert_eq!(projected.skills.iter().find(|s| s.type_id == skill.type_id).unwrap().skill_points, banked);
                        assert_eq!(projected.queue[0].start_time, Some(expiry));
                        assert!(projected.queue[0].end_time.unwrap() > old_end);
                        assert!(projected.trained_points().unwrap_or_else(|_| panic!("projecting points")) <= baseline + training::points_at(0, old_row.dest_sp, start, old_end, expiry + crate::consts::FILETIME_SECOND));
                        let higher = banked + 1;
                        db.execute("UPDATE game.skills SET skill_points=$3 WHERE character_id=$1 AND type_id=$2", &[&f.character, &skill.type_id, &higher]).await.unwrap();
                        let already_banked = plan(&db, &c.sde, f.character, expiry).await;
                        assert_eq!(already_banked.queue[0].start_sp, higher);
                        assert_eq!(already_banked.skills.iter().find(|s| s.type_id == skill.type_id).unwrap().skill_points, higher);
                        assert!(crate::dao::skills::queue::list(&db, f.character).await.unwrap() == vec![old_row]);
                        assert!(crate::dao::items::brain::get(&db, f.character, item).await.unwrap().is_some());
                        exercised = true;
                        break;
                    }
                }
                db.execute("DELETE FROM game.items WHERE item_id=$1", &[&item]).await.unwrap();
                if exercised { break 'types; }
            }
        }
    }
    fixture::cleanup(&db, f).await.unwrap();
    assert!(exercised, "no training booster and compatible skill found in test SDE");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn completion_precedes_equal_expiry_and_harmless_expiries_preserve_queue() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let existing = crate::dao::skills::training::current(&db, f.character).await.unwrap();
    let skill = c.sde.types.iter().find(|s| s.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some() && !existing.iter().any(|r| r.type_id == s.type_id)).unwrap();
    crate::dao::skills::training::starting_skill(&db, f.character, skill.type_id, 0, 0).await.unwrap();
    let first = QueueEntry { position: 0, type_id: skill.type_id, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(100) };
    let second = QueueEntry { position: 1, type_id: skill.type_id, to_level: 2, start_sp: 100, dest_sp: 200, start_time: Some(100), end_time: Some(300) };
    crate::dao::skills::queue::insert(&db, f.character, &first).await.unwrap();
    crate::dao::skills::queue::insert(&db, f.character, &second).await.unwrap();
    for expiry in [100i64, 100, 200] {
        db.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at) VALUES (1,$1,$1,$2,true,$3)", &[&f.character, &FLAG_BOOSTER, &expiry]).await.unwrap();
    }
    let plan = plan(&db, &c.sde, f.character, 250).await;
    assert!(matches!(&plan.events[0], Event::Completion(c) if c.end_time == 100 && c.type_id == skill.type_id));
    assert!(matches!(&plan.events[1], Event::Expiry { at: 100, items } if items.len() == 2));
    assert!(matches!(&plan.events[2], Event::Expiry { at: 200, items } if items.len() == 1));
    let mut expected = second.clone();
    expected.position = 0;
    assert!(plan.queue == vec![expected]);
    assert!(crate::dao::skills::queue::list(&db, f.character).await.unwrap() == vec![first, second]);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn expiry_before_and_after_completion_and_empty_queue() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    crate::dao::skills::training::starting_skill(&db, f.character, 1, 0, 0).await.unwrap();
    let row = QueueEntry { position: 0, type_id: 1, to_level: 1, start_sp: 0, dest_sp: 100, start_time: Some(1), end_time: Some(200) };
    crate::dao::skills::queue::insert(&db, f.character, &row).await.unwrap();
    for expiry in [100i64, 300] {
        db.execute("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,singleton,brain_expires_at) VALUES (1,$1,$1,$2,true,$3)", &[&f.character, &FLAG_BOOSTER, &expiry]).await.unwrap();
    }
    let before = fingerprint(&db, f.character).await;
    let active = plan(&db, &c.sde, f.character, 150).await;
    assert!(matches!(&active.events[..], [Event::Expiry { at: 100, .. }]));
    assert!(active.queue == vec![row.clone()]);
    assert!(!active.queue_changed && active.brain_changed && active.boosters_changed);
    let all = plan(&db, &c.sde, f.character, 350).await;
    assert!(matches!(&all.events[..], [Event::Expiry { at: 100, .. }, Event::Completion(c), Event::Expiry { at: 300, .. }] if c.end_time == 200));
    assert!(all.queue.is_empty());
    assert_eq!(fingerprint(&db, f.character).await, before);
    db.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&f.character]).await.unwrap();
    let empty = plan(&db, &c.sde, f.character, 350).await;
    assert!(matches!(&empty.events[..], [Event::Expiry { at: 100, .. }, Event::Expiry { at: 300, .. }]));
    assert!(!empty.queue_changed && empty.brain_changed && empty.boosters_changed);
    fixture::cleanup(&db, f).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn supplied_skill_points_change_an_applicable_modifier() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let mut found = false;
    'skills: for data in c.sde.types.iter() {
        let Some(rank) = data.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT) else { continue };
        if rank <= 0.0 { continue; }
        for entry in data.effects() {
            let Some(effect) = c.sde.dogma.effect(entry.effect_id) else { continue };
            for modifier in effect.modifiers() {
                let low = [Skill { type_id: data.type_id, level: 1, skill_points: super::super::formulas::sp_for_level(rank, 1) }];
                let high = [Skill { type_id: data.type_id, level: 5, skill_points: super::super::formulas::sp_for_level(rank, 5) }];
                let a = Fit::load_character_with_skills_at(&db, c.sde.clone(), f.character, 100, &low, &[]).await.unwrap_or_else(|_| panic!("loading lower skill"));
                let b = Fit::load_character_with_skills_at(&db, c.sde.clone(), f.character, 100, &high, &[]).await.unwrap_or_else(|_| panic!("loading higher skill"));
                if a.value(Key::Char, modifier.modified) != b.value(Key::Char, modifier.modified) {
                    found = true;
                    break 'skills;
                }
            }
        }
    }
    fixture::cleanup(&db, f).await.unwrap();
    assert!(found, "test SDE has no applicable level-sensitive character modifier");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn training_precedes_expert_expiry_at_equal_time() {
    use crate::services::tests::{client, fixture};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let (c, _peer) = client(db.clone(), f.character).await;
    let skill = c.sde.types.iter().find(|ty| ty.attribute(crate::consts::dogma::ATTR_SKILL_TIME_CONSTANT).is_some()).unwrap().type_id;
    crate::dao::skills::training::starting_skill(&db, f.character, skill, 0, 0).await.unwrap();
    crate::dao::skills::queue::insert(&db, f.character, &QueueEntry { position: 0, type_id: skill, to_level: 5, start_sp: 0, dest_sp: 100, start_time: Some(10), end_time: Some(20) }).await.unwrap();
    crate::dao::skills::expert_systems::insert(&db, f.character, 1, 10, 20).await.unwrap();
    let projected = plan(&db, &c.sde, f.character, 20).await;
    assert!(matches!(projected.events.as_slice(), [Event::Completion(_), Event::ExpertExpiry { .. }]));
    assert_eq!(projected.initial_installations.len(), 1);
    db.execute("DELETE FROM game.character_expert_systems WHERE character_id=$1", &[&f.character]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
}
