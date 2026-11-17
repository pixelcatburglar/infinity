//! Corporation plan request routing, authority, wire shapes and notices on a real database.
use prost_reflect::{ReflectMessage, Value as PV};
use crate::{dao::skills::plans::{self, Owner}, gateway::{protocol::target_group::Target, skill_plans::personal}, services::tests::fixture};
use super::handle;
use super::super::tests::{assign, category_request, context, corporation, refused, request, response_uuid, set_requirements, uuid_request, MANAGER_ROLE};

const CREATE: &str = "eve_public.corporation.skill.plan.CreateRequest";
const GET_ALL: &str = "eve_public.corporation.skill.plan.GetAllRequest";
const GET: &str = "eve_public.corporation.skill.plan.GetRequest";
const SHARED: &str = "eve_public.corporation.skill.plan.GetSharedRequest";
const DELETE: &str = "eve_public.corporation.skill.plan.DeleteRequest";
const SET_NAME: &str = "eve_public.corporation.skill.plan.SetNameRequest";
const SET_DESCRIPTION: &str = "eve_public.corporation.skill.plan.SetDescriptionRequest";
const SET_REQUIREMENTS: &str = "eve_public.corporation.skill.plan.SetSkillRequirementsRequest";
const SET_CATEGORY: &str = "eve_public.corporation.skill.plan.SetCategoryRequest";
const PERSONAL_GET: &str = "eve_public.character.skill.plan.GetRequest";
const PERSONAL_SHARED: &str = "eve_public.character.skill.plan.GetSharedRequest";

/// A career-path category encoded the way the client writes `uuid.UUID(int=id)`.
async fn category(db: &crate::db::Db) -> Vec<u8> {
    let id: i64 = db.query_one("SELECT key::bigint AS id FROM sde_native.careerpaths ORDER BY key LIMIT 1", &[]).await.unwrap().get("id");
    let mut category = vec![0u8; 16];
    category[12..].copy_from_slice(&(id as u32).to_be_bytes());
    category
}

fn create_request(name: &str, category: &[u8]) -> prost_reflect::DynamicMessage {
    let mut req = request(CREATE);
    let mut plan = crate::gateway::message::child(&req.descriptor(), "skill_plan").unwrap();
    crate::gateway::message::set(&mut plan, "name", PV::String(name.into())).unwrap();
    crate::gateway::message::set(&mut plan, "description", PV::String("corporation plan".into())).unwrap();
    let mut cat = crate::gateway::message::child(&plan.descriptor(), "category").unwrap();
    crate::gateway::message::set(&mut cat, "uuid", PV::Bytes(bytes::Bytes::from(category.to_vec()))).unwrap();
    crate::gateway::message::set(&mut plan, "category", PV::Message(cat)).unwrap();
    crate::gateway::message::set(&mut req, "skill_plan", PV::Message(plan)).unwrap();
    req
}

/// Member session plumbing: an online pilot bound to a notice stream application.
fn online(gw: &crate::gateway::Gateway, member: &fixture::Fixture, corp: i64) -> tokio::sync::mpsc::UnboundedReceiver<(String, Vec<marshal::Value>)> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    gw.world.register_pilot(member.character, tx);
    gw.world.set_online_session(member.character, Some(infinity_core::session::OnlineSession {
        character_id: member.character, user_id: Some(member.account), session_id: 1, ship_id: None,
        station_id: None, system_id: None, corporation_id: Some(corp), alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    rx
}
async fn cleanup(db: &crate::db::Db, corp: i64, fixtures: &[fixture::Fixture]) {
    db.execute("DELETE FROM game.skill_plans WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=ANY($1)", &[&fixtures.iter().map(|f| f.character).collect::<Vec<_>>()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for f in fixtures { fixture::cleanup(db, *f).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn corporation_plan_requests_round_trip_and_notices() {
    let gw = crate::gateway::tests::gateway().await;
    let manager = fixture::create(&gw.db).await.unwrap();
    let member = fixture::create(&gw.db).await.unwrap();
    let outsider = fixture::create(&gw.db).await.unwrap();
    let corp = corporation(&gw.db, manager.character).await;
    assign(&gw.db, member.character, corp, 0, 0, 0, false).await;
    assign(&gw.db, manager.character, corp, MANAGER_ROLE, 0, 0, false).await;
    let _receiver = online(&gw, &member, corp);
    let mut stream = crate::gateway::tests::notice_stream(&gw.world, [7; 16], member.character, 8);
    let ctx = context(&gw, manager.account, manager.character).await;
    let member_ctx = context(&gw, member.account, member.character).await;
    let outsider_ctx = context(&gw, outsider.account, outsider.character).await;
    let category = category(&gw.db).await;
    let skill = ctx.sde.types.iter().find(|t| crate::services::skill_plans::validation::valid_skill(ctx.sde, t.type_id) && ctx.sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;

    let mut create = create_request("Wing plans", &category);
    let mut plan = create.get_field_by_name("skill_plan").unwrap().as_message().cloned().unwrap();
    set_requirements(&mut plan, "skill_requirements", &[(skill as u64, 1)]);
    crate::gateway::message::set(&mut create, "skill_plan", PV::Message(plan)).unwrap();
    let created = handle(&ctx, CREATE, &create).await.unwrap();
    let plan_id = response_uuid(&created, "skill_plan");
    let (notice, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), "eve_public.corporation.skill.plan.CreatedNotice");
    assert!(matches!(notice.target_group.unwrap().target, Some(Target::Corporation(id)) if i64::from(id) == corp));
    assert_eq!(response_uuid(&payload, "identifier"), plan_id);
    let attrs_field = payload.get_field_by_name("skill_plan").unwrap();
    let attrs = attrs_field.as_message().unwrap();
    assert_eq!(attrs.get_field_by_name("name").unwrap().as_str(), Some("Wing plans"));
    assert_eq!(attrs.get_field_by_name("description").unwrap().as_str(), Some("corporation plan"));
    assert_eq!(attrs.get_field_by_name("skill_requirements").unwrap().as_list().unwrap().len(), 1);
    assert_eq!(response_uuid(attrs, "category"), category);
    let row = gw.db.query_one("SELECT corporation_id, category_id FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("corporation_id"), corp);
    assert_eq!(row.get::<_, Vec<u8>>("category_id"), category);

    let all = handle(&member_ctx, GET_ALL, &request(GET_ALL)).await.unwrap();
    let list = all.get_field_by_name("skill_plans").unwrap();
    let entries = list.as_list().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = entries[0].as_message().unwrap();
    assert_eq!(response_uuid(entry, "identifier"), plan_id);
    let summary_field = entry.get_field_by_name("skill_plan").unwrap();
    let summary = summary_field.as_message().unwrap();
    assert_eq!(summary.descriptor().full_name(), "eve_public.corporation.skill.plan.Summary");
    assert_eq!(summary.get_field_by_name("name").unwrap().as_str(), Some("Wing plans"));
    let got = handle(&member_ctx, GET, &uuid_request(GET, &plan_id)).await.unwrap();
    let attrs_field = got.get_field_by_name("skill_plan").unwrap();
    let attrs = attrs_field.as_message().unwrap();
    assert_eq!(attrs.descriptor().full_name(), "eve_public.corporation.skill.plan.Attributes");
    assert_eq!(attrs.get_field_by_name("skill_requirements").unwrap().as_list().unwrap().len(), 1);
    let shared = handle(&member_ctx, SHARED, &uuid_request(SHARED, &plan_id)).await.unwrap();
    assert!(shared.has_field_by_name("skill_plan"));
    let foreign = handle(&outsider_ctx, SHARED, &uuid_request(SHARED, &plan_id)).await.unwrap();
    assert!(!foreign.has_field_by_name("skill_plan"));
    refused(handle(&outsider_ctx, GET, &uuid_request(GET, &plan_id)).await.unwrap_err(), crate::gateway::error::FailureCode::NotFound);

    let mut rename = uuid_request(SET_NAME, &plan_id);
    crate::gateway::message::set(&mut rename, "name", PV::String("Fleet plans".into())).unwrap();
    handle(&ctx, SET_NAME, &rename).await.unwrap();
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), "eve_public.corporation.skill.plan.NameUpdatedNotice");
    assert_eq!(payload.get_field_by_name("name").unwrap().as_str(), Some("Fleet plans"));
    assert_eq!(response_uuid(&payload, "identifier"), plan_id);
    let mut describe = uuid_request(SET_DESCRIPTION, &plan_id);
    crate::gateway::message::set(&mut describe, "description", PV::String("new text".into())).unwrap();
    handle(&ctx, SET_DESCRIPTION, &describe).await.unwrap();
    let mut requirements = uuid_request(SET_REQUIREMENTS, &plan_id);
    set_requirements(&mut requirements, "requirements", &[(skill as u64, 1), (skill as u64, 2)]);
    handle(&ctx, SET_REQUIREMENTS, &requirements).await.unwrap();
    handle(&ctx, SET_CATEGORY, &category_request(SET_CATEGORY, &plan_id, &[0; 16])).await.unwrap();
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), "eve_public.corporation.skill.plan.DescriptionUpdatedNotice");
    assert_eq!(response_uuid(&payload, "identifier"), plan_id);
    assert_eq!(payload.get_field_by_name("description").unwrap().as_str(), Some("new text"));
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), "eve_public.corporation.skill.plan.SkillRequirementsUpdatedNotice");
    assert_eq!(response_uuid(&payload, "identifier"), plan_id);
    let pairs: Vec<(u64, u32)> = payload.get_field_by_name("requirements").unwrap().as_list().unwrap().iter()
        .map(|value| {
            let entry = value.as_message().unwrap();
            (entry.get_field_by_name("skill_type").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u64().unwrap(),
             entry.get_field_by_name("level").unwrap().as_u32().unwrap())
        }).collect();
    assert_eq!(pairs, [(skill as u64, 1), (skill as u64, 2)]);
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), "eve_public.corporation.skill.plan.CategoryUpdatedNotice");
    assert_eq!(response_uuid(&payload, "identifier"), plan_id);
    assert_eq!(response_uuid(&payload, "category"), vec![0u8; 16]);
    let row = gw.db.query_one("SELECT name, description, skill_ids, levels, category_id FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
    assert_eq!((row.get::<_, String>("name"), row.get::<_, String>("description")), ("Fleet plans".to_string(), "new text".to_string()));
    assert_eq!((row.get::<_, Vec<i32>>("skill_ids"), row.get::<_, Vec<i32>>("levels")), (vec![skill, skill], vec![1, 2]));
    assert_eq!(row.get::<_, Vec<u8>>("category_id"), vec![0u8; 16]);

    refused(handle(&member_ctx, SET_NAME, &rename).await.unwrap_err(), crate::gateway::error::FailureCode::PreconditionFailed);
    refused(handle(&outsider_ctx, DELETE, &uuid_request(DELETE, &plan_id)).await.unwrap_err(), crate::gateway::error::FailureCode::PreconditionFailed);
    assert!(!personal::skill_plan(&member_ctx, PERSONAL_SHARED, &uuid_request(PERSONAL_SHARED, &plan_id)).await.unwrap().has_field_by_name("skill_plan"));
    refused(personal::skill_plan(&member_ctx, PERSONAL_GET, &uuid_request(PERSONAL_GET, &plan_id)).await.unwrap_err(), crate::gateway::error::FailureCode::NotFound);
    // Refused writes roll back before publish: nothing reaches the member's stream.
    assert!(stream.rx.try_recv().is_err());

    handle(&ctx, DELETE, &uuid_request(DELETE, &plan_id)).await.unwrap();
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), "eve_public.corporation.skill.plan.DeletedNotice");
    assert_eq!(response_uuid(&payload, "skill_plan"), plan_id);
    assert!(plans::get(&gw.db, &plan_id, None).await.unwrap().is_none());
    refused(handle(&ctx, GET, &uuid_request(GET, &[1, 2, 3])).await.unwrap_err(), crate::gateway::error::FailureCode::BadRequest);
    cleanup(&gw.db, corp, &[manager, member, outsider]).await;
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn corporation_plan_npc_and_quota_boundaries() {
    let gw = crate::gateway::tests::gateway().await;
    let manager = fixture::create(&gw.db).await.unwrap();
    let npc = fixture::create(&gw.db).await.unwrap();
    let corp = corporation(&gw.db, manager.character).await;
    assign(&gw.db, manager.character, corp, MANAGER_ROLE, 0, 0, false).await;
    assign(&gw.db, npc.character, 1_000_000, 0, 0, 0, false).await;
    let ctx = context(&gw, manager.account, manager.character).await;
    let npc_ctx = context(&gw, npc.account, npc.character).await;
    let category = category(&gw.db).await;

    // An NPC-corp member is a normal reader: 200 empty list, absent shared, no plans of their own.
    let all = handle(&npc_ctx, GET_ALL, &request(GET_ALL)).await.unwrap();
    assert!(all.get_field_by_name("skill_plans").unwrap().as_list().unwrap().is_empty());
    let shared = handle(&npc_ctx, SHARED, &uuid_request(SHARED, &[9; 16])).await.unwrap();
    assert!(!shared.has_field_by_name("skill_plan"));
    refused(handle(&npc_ctx, CREATE, &create_request("npc", &category)).await.unwrap_err(), crate::gateway::error::FailureCode::PreconditionFailed);

    for _ in 0..99 { plans::create(&gw.db, &super::super::tests::uuid_bytes(), Owner::Corporation(corp), "filler", "", &[], &[], Some(&category)).await.unwrap(); }
    handle(&ctx, CREATE, &create_request("last", &category)).await.unwrap();
    refused(handle(&ctx, CREATE, &create_request("over", &category)).await.unwrap_err(), crate::gateway::error::FailureCode::PreconditionFailed);
    assert_eq!(plans::all(&gw.db, Owner::Corporation(corp)).await.unwrap().len(), 100);
    cleanup(&gw.db, corp, &[manager, npc]).await;
}
