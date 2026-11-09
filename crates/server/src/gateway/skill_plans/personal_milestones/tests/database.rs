use super::super::{handle, uuid};
use crate::gateway::{Caller, context::HandlerContext, message::message};
use crate::services::tests::fixture;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::dao::skills::plans;
use crate::gateway::{error::{FailureCode, GatewayError}, message::{child, identifier, new_uuid, set}};
use crate::services::skill_plans::validation;

fn request(name: &str) -> DynamicMessage { DynamicMessage::new(message(name).unwrap()) }
fn attach_uuid(message: &mut DynamicMessage, field: &str, id: &[u8]) {
    let mut value = child(&message.descriptor(), field).unwrap();
    set(&mut value, "uuid", PV::Bytes(Bytes::copy_from_slice(id))).unwrap();
    set(message, field, PV::Message(value)).unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn personal_milestone_create_share_update_delete() {
    let gateway = crate::gateway::tests::gateway().await;
    let owner = fixture::create(&gateway.db).await.unwrap();
    let viewer = fixture::create(&gateway.db).await.unwrap();
    let owner_ctx = HandlerContext::for_request(&gateway, Caller { user_id: Some(owner.account), character_id: Some(owner.character) }).await.unwrap();
    let viewer_ctx = HandlerContext::for_request(&gateway, Caller { user_id: Some(viewer.account), character_id: Some(viewer.character) }).await.unwrap();
    let skill = owner_ctx.sde.types.iter().find(|t| validation::valid_skill(owner_ctx.sde, t.type_id) && owner_ctx.sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let points = 1_000_000_000_i64;
    gateway.db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3)", &[&owner.character, &skill, &points]).await.unwrap();
    let plan = new_uuid();
    plans::create(&gateway.db, &plan, owner.character, "plan", "", &[skill], &[1]).await.unwrap();
    let create = "eve_public.character.skill.plan.milestone.CreateRequest";
    let mut req = request(create);
    let mut data = child(&req.descriptor(), "milestone").unwrap();
    attach_uuid(&mut data, "skill_plan", &plan);
    let mut skill_req = child(&data.descriptor(), "skill").unwrap();
    let type_id = identifier(&skill_req.descriptor(), "skill_type", PV::U64(skill as u64)).unwrap();
    set(&mut skill_req, "skill_type", PV::Message(type_id)).unwrap();
    set(&mut skill_req, "level", PV::U32(1)).unwrap();
    set(&mut data, "skill", PV::Message(skill_req)).unwrap();
    set(&mut req, "milestone", PV::Message(data)).unwrap();
    for (level, code) in [(0, FailureCode::PreconditionFailed), (6, FailureCode::PreconditionFailed), (u32::MAX, FailureCode::BadRequest)] {
        let mut bad = req.clone();
        let mut data = bad.get_field_by_name("milestone").unwrap().as_message().unwrap().clone();
        let mut target = data.get_field_by_name("skill").unwrap().as_message().unwrap().clone();
        set(&mut target, "level", PV::U32(level)).unwrap();
        set(&mut data, "skill", PV::Message(target)).unwrap();
        set(&mut bad, "milestone", PV::Message(data)).unwrap();
        assert!(matches!(handle(&owner_ctx, create, &bad).await, Err(GatewayError::Refused { code: actual, .. }) if std::mem::discriminant(&actual) == std::mem::discriminant(&code)));
    }
    let response = handle(&owner_ctx, create, &req).await.unwrap();
    let milestone = uuid(&response, "milestone").unwrap();
    assert!(matches!(handle(&owner_ctx, create, &req).await, Err(GatewayError::Refused { code: FailureCode::PreconditionFailed, .. })));
    let get = "eve_public.character.skill.plan.milestone.GetAllRequest";
    let mut read = request(get); attach_uuid(&mut read, "skill_plan", &plan);
    let response = handle(&viewer_ctx, get, &read).await.unwrap();
    assert_eq!(response.get_field_by_name("milestones").unwrap().as_list().unwrap().len(), 1);
    let count: i64 = gateway.db.query_one("SELECT count(*) AS n FROM game.skill_plan_milestone_progress WHERE milestone_id=$1", &[&milestone]).await.unwrap().get("n");
    assert_eq!(count, 2);
    let owner_progress = gateway.db.query_one("SELECT completed,trained_sp,required_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    let viewer_progress = gateway.db.query_one("SELECT completed,trained_sp FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&viewer.character, &milestone]).await.unwrap();
    assert!(owner_progress.get::<_, bool>("completed"));
    assert_eq!(owner_progress.get::<_, i64>("trained_sp"), owner_progress.get::<_, i64>("required_sp"));
    assert!(!viewer_progress.get::<_, bool>("completed"));
    assert_eq!(viewer_progress.get::<_, i64>("trained_sp"), 0);
    gateway.db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,1,$3)", &[&viewer.character, &skill, &points]).await.unwrap();
    handle(&viewer_ctx, get, &read).await.unwrap();
    let repaired = gateway.db.query_one("SELECT completed FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&viewer.character, &milestone]).await.unwrap();
    assert!(repaired.get::<_, bool>("completed"));
    let set_description = "eve_public.character.skill.plan.milestone.SetDescriptionRequest";
    let mut update = request(set_description); attach_uuid(&mut update, "identifier", &milestone);
    let description = format!("{}a\u{301}", "🚀".repeat(1000));
    set(&mut update, "description", PV::String(description.clone())).unwrap();
    assert!(matches!(handle(&viewer_ctx, set_description, &update).await, Err(GatewayError::Refused { code: FailureCode::NotFound, .. })));
    handle(&owner_ctx, set_description, &update).await.unwrap();
    let response = handle(&viewer_ctx, get, &read).await.unwrap();
    let entries = response.get_field_by_name("milestones").unwrap();
    let data = entries.as_list().unwrap()[0].as_message().unwrap().get_field_by_name("data").unwrap();
    assert_eq!(data.as_message().unwrap().get_field_by_name("description").unwrap().as_str(), Some(description.as_str()));
    let delete = "eve_public.character.skill.plan.milestone.DeleteRequest";
    let mut remove = request(delete); attach_uuid(&mut remove, "milestone", &milestone);
    handle(&owner_ctx, delete, &remove).await.unwrap();
    assert!(gateway.db.query_opt("SELECT 1 FROM game.skill_plan_milestone_progress WHERE milestone_id=$1", &[&milestone]).await.unwrap().is_none());
    let replacement = handle(&owner_ctx, create, &req).await.unwrap();
    let replacement_id = uuid(&replacement, "milestone").unwrap();
    handle(&viewer_ctx, get, &read).await.unwrap();
    plans::delete(&gateway.db, owner.character, &plan).await.unwrap();
    assert!(gateway.db.query_opt("SELECT 1 FROM game.skill_plan_milestones WHERE milestone_id=$1", &[&replacement_id]).await.unwrap().is_none());
    assert!(gateway.db.query_opt("SELECT 1 FROM game.skill_plan_milestone_progress WHERE milestone_id=$1", &[&replacement_id]).await.unwrap().is_none());
    fixture::cleanup(&gateway.db, owner).await.unwrap();
    fixture::cleanup(&gateway.db, viewer).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn concurrent_milestone_creates_obey_five_milestone_cap() {
    let gateway = crate::gateway::tests::gateway().await;
    let owner = fixture::create(&gateway.db).await.unwrap();
    let ctx = HandlerContext::for_request(&gateway, Caller { user_id: Some(owner.account), character_id: Some(owner.character) }).await.unwrap();
    let skills: Vec<_> = ctx.sde.types.iter().filter(|t| validation::valid_skill(ctx.sde, t.type_id) && ctx.sde.skill_plans.requirements(t.type_id).is_empty()).take(2).map(|t| t.type_id).collect();
    assert_eq!(skills.len(), 2);
    let plan = new_uuid();
    plans::create(&gateway.db, &plan, owner.character, "plan", "", &[skills[0], skills[0], skills[0], skills[0], skills[0], skills[1]], &[1, 2, 3, 4, 5, 1]).await.unwrap();
    let create = "eve_public.character.skill.plan.milestone.CreateRequest";
    let make_request = |skill: i32, level: u32| {
        let mut req = request(create);
        let mut data = child(&req.descriptor(), "milestone").unwrap();
        attach_uuid(&mut data, "skill_plan", &plan);
        let mut target = child(&data.descriptor(), "skill").unwrap();
        let type_id = identifier(&target.descriptor(), "skill_type", PV::U64(skill as u64)).unwrap();
        set(&mut target, "skill_type", PV::Message(type_id)).unwrap();
        set(&mut target, "level", PV::U32(level)).unwrap();
        set(&mut data, "skill", PV::Message(target)).unwrap();
        set(&mut req, "milestone", PV::Message(data)).unwrap();
        req
    };
    for level in 1..=4 {
        handle(&ctx, create, &make_request(skills[0], level)).await.unwrap();
    }
    let first = make_request(skills[0], 5);
    let second = make_request(skills[1], 1);
    let (first, second) = tokio::join!(handle(&ctx, create, &first), handle(&ctx, create, &second));
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    assert!(matches!(first.err().or_else(|| second.err()).unwrap(), GatewayError::Refused { code: FailureCode::PreconditionFailed, .. }));
    let count: i64 = gateway.db.query_one("SELECT count(*) AS n FROM game.skill_plan_milestones WHERE plan_id=$1", &[&plan]).await.unwrap().get("n");
    assert_eq!(count, 5);
    plans::delete(&gateway.db, owner.character, &plan).await.unwrap();
    fixture::cleanup(&gateway.db, owner).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn milestone_create_settles_due_training_before_snapshot() {
    let gateway = crate::gateway::tests::gateway().await;
    let owner = fixture::create(&gateway.db).await.unwrap();
    let ctx = HandlerContext::for_request(&gateway, Caller { user_id: Some(owner.account), character_id: Some(owner.character) }).await.unwrap();
    let skill = ctx.sde.types.iter().find(|t| validation::valid_skill(ctx.sde, t.type_id) && ctx.sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;
    let plan = new_uuid();
    plans::create(&gateway.db, &plan, owner.character, "plan", "", &[skill], &[1]).await.unwrap();
    gateway.db.execute("INSERT INTO game.skills(character_id,type_id,level,skill_points) VALUES ($1,$2,0,0)", &[&owner.character, &skill]).await.unwrap();
    gateway.db.execute("INSERT INTO game.skill_queue(character_id,position,type_id,to_level,start_sp,dest_sp,start_time,end_time) VALUES ($1,0,$2,1,0,250,1,2)", &[&owner.character, &skill]).await.unwrap();
    let create = "eve_public.character.skill.plan.milestone.CreateRequest";
    let mut req = request(create);
    let mut data = child(&req.descriptor(), "milestone").unwrap();
    attach_uuid(&mut data, "skill_plan", &plan);
    let mut target = child(&data.descriptor(), "skill").unwrap();
    let type_id = identifier(&target.descriptor(), "skill_type", PV::U64(skill as u64)).unwrap();
    set(&mut target, "skill_type", PV::Message(type_id)).unwrap();
    set(&mut target, "level", PV::U32(1)).unwrap();
    set(&mut data, "skill", PV::Message(target)).unwrap();
    set(&mut req, "milestone", PV::Message(data)).unwrap();
    let response = handle(&ctx, create, &req).await.unwrap();
    let milestone = uuid(&response, "milestone").unwrap();
    let row = gateway.db.query_one("SELECT completed FROM game.skill_plan_milestone_progress WHERE character_id=$1 AND milestone_id=$2", &[&owner.character, &milestone]).await.unwrap();
    assert!(row.get::<_, bool>("completed"));
    assert_eq!(gateway.db.query_one("SELECT level FROM game.skills WHERE character_id=$1 AND type_id=$2", &[&owner.character, &skill]).await.unwrap().get::<_, i32>("level"), 1);
    plans::delete(&gateway.db, owner.character, &plan).await.unwrap();
    gateway.db.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&owner.character]).await.unwrap();
    fixture::cleanup(&gateway.db, owner).await.unwrap();
}
