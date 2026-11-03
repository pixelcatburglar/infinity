//! Registered Lua skill queue boundaries and persistent training behavior.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn queue_roundtrip_offline_online_and_rollback() {
    let r = fixture::resources().await;
    let (race, bloodline, school, station) = fixture::creation_inputs(&r).await;
    let lua = Lua::new();
    register(&lua, &lua.globals(), &r).unwrap();
    let globals = lua.globals();
    globals.set("race", race).unwrap(); globals.set("bloodline", bloodline).unwrap();
    globals.set("school", school).unwrap(); globals.set("station", station).unwrap();
    globals.set("pilot_name", format!("SkillPilot{}", crate::space::now_ms())).unwrap();
    let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("id", id).unwrap();
    let test_r = r.clone();
    let result = tokio::spawn(async move {
        let r = test_r;
        let skills = crate::dao::skills::training::current(&r.db, id).await.unwrap();
        let skill = skills.iter().find(|s| s.level < 5 && s.skill_points > 0).expect("trainable starting skill");
        globals.set("skill", skill.type_id).unwrap(); globals.set("level", skill.level + 1).unwrap();
        lua.load("pilots.save_skill_queue(id, {{type_id=skill,level=level}})").exec_async().await.unwrap();
        lua.load(r#"
            local q = pilots.skill_queue(id)
            assert(#q == 1 and q[1].type_id == skill and q[1].level == level and q[1].position == 0)
            assert(math.type(q[1].type_id) == 'integer' and math.type(q[1].level) == 'integer')
            assert(math.type(q[1].start_sp) == 'integer' and math.type(q[1].dest_sp) == 'integer')
            assert(math.type(q[1].start_time) == 'integer' and math.type(q[1].end_time) == 'integer')
            assert(q[1].trainingTypeID == nil)
        "#).exec_async().await.unwrap();
        let before = crate::dao::skills::queue::list(&r.db, id).await.unwrap();
        for bad in [
            "{[2]={type_id=skill,level=level}}", "{[1]={type_id=skill,level=level},extra=1}",
            "{{type_id=skill,level=level,extra=1}}", "{{type_id=skill}}", "{1}",
            "{{type_id=0,level=1}}", "{{type_id=skill,level=1.5}}", "{{type_id=2147483648,level=1}}",
            "{{type_id=skill,level='1'}}", "{{type_id=skill,level=0}}",
            "{{type_id=2147483647,level=1}}",
        ] {
            let call = format!("pilots.save_skill_queue(id, {bad})");
            assert!(lua.load(&call).exec_async().await.is_err(), "{call}");
            assert!(crate::dao::skills::queue::list(&r.db, id).await.unwrap() == before);
        }
        assert!(lua.load("return pilots.skill_queue(0)").eval_async::<Value>().await.is_err());
        lua.load("pilots.online(id)").exec_async().await.unwrap();
        lua.load("local q=pilots.skill_queue(id); assert(#q == 1 and q[1].type_id == skill and q[1].level == level)").exec_async().await.unwrap();
        lua.load("pilots.save_skill_queue(id, {})").exec_async().await.unwrap();
        assert!(crate::dao::skills::queue::list(&r.db, id).await.unwrap().is_empty());
        let job = format!("{}:{id}", crate::services::skills::completion::KIND);
        assert!(r.db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap().is_none());
    }).await;
    let cleanup_lua = Lua::new();
    register(&cleanup_lua, &cleanup_lua.globals(), &r).unwrap();
    cleanup_lua.globals().set("id", id).unwrap();
    let cleared = cleanup_lua.load("pilots.save_skill_queue(id, {})").exec_async().await;
    if cleared.is_err() {
        r.db.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&id]).await.unwrap();
        let job = format!("{}:{id}", crate::services::skills::completion::KIND);
        r.db.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&job]).await.unwrap();
    }
    let _ = lifecycle::offline(r.clone(), id).await;
    r.db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&id]).await.unwrap();
    fixture::cleanup(&r.db, id).await;
    result.unwrap();
    cleared.unwrap();
}
