//! Expert deadlines share the durable skill completion job.
use super::*;

#[tokio::test]
#[ignore = "requires a dedicated INFINITY_TEST_DATABASE_URL with SDE and no foreign seedable skill state"]
async fn restart_seeds_overdue_and_future_expert_deadlines() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    require_no_seedable_skill_state(&db).await;
    let f = fixture::create(&db).await.unwrap();
    let now = filetime_now();
    let first = now + infinity_core::consts::FILETIME_MINUTE;
    let second = first + infinity_core::consts::FILETIME_MINUTE;
    let owner = f.character;
    db.execute("INSERT INTO game.character_expert_systems (character_id,type_id,installed_at,expires_at) VALUES ($1,1,$2,$3),($1,2,$2,$4),($1,3,$2,$5)", &[&owner, &(now - 100), &(now - 1), &first, &second]).await.unwrap();
    seed(&db).await.unwrap_or_else(|_| panic!("seeding expert deadline"));
    let id = format!("{KIND}:{owner}");
    let due: i64 = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap().get("due_at");
    assert_eq!(due, now - 1);
    db.execute("DELETE FROM game.character_expert_systems WHERE character_id=$1 AND type_id=1", &[&owner]).await.unwrap();
    reschedule(&db, owner).await.unwrap_or_else(|_| panic!("rescheduling future expert"));
    let due: i64 = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap().get("due_at");
    assert_eq!(due, first);
    db.execute("DELETE FROM game.character_expert_systems WHERE character_id=$1", &[&owner]).await.unwrap();
    reschedule(&db, owner).await.unwrap_or_else(|_| panic!("cancelling expert deadline"));
    assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&id]).await.unwrap().is_none());
    fixture::cleanup(&db, f).await.unwrap();
}
