//! Corporation milestone request routing: both target kinds, description and
//! five-slot replacement, cascade and membership, on a real database.
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{
    dao::skills::{plan_milestones, plans::{self, Owner}},
    gateway::{error::FailureCode, message::{child, identifier, set}, protocol::target_group::Target},
    services::{skill_plans::validation, tests::fixture},
};
use super::handle;
use super::super::tests::{assign, context, corporation, refused, request, response_uuid, uuid_bytes, MANAGER_ROLE};

const PREFIX: &str = "eve_public.corporation.skill.plan.milestone";

/// `id` into the `uuid` field of the `Identifier` message at `m.field`.
fn attach(m: &mut DynamicMessage, field: &str, id: &[u8]) {
    let mut value = child(&m.descriptor(), field).unwrap();
    set(&mut value, "uuid", PV::Bytes(bytes::Bytes::from(id.to_vec()))).unwrap();
    set(m, field, PV::Message(value)).unwrap();
}

/// A `CreateRequest` carrying one `milestone` with `skill_plan` parent and one
/// oneof arm: `skill{skill_type, level}` or `train_to_type`.
fn create_request(parent: &[u8], type_id: u64, level: Option<u32>) -> DynamicMessage {
    let mut req = request(&format!("{PREFIX}.CreateRequest"));
    let mut data = child(&req.descriptor(), "milestone").unwrap();
    attach(&mut data, "skill_plan", parent);
    if let Some(level) = level {
        let mut skill = child(&data.descriptor(), "skill").unwrap();
        let skill_type = identifier(&skill.descriptor(), "skill_type", PV::U64(type_id)).unwrap();
        set(&mut skill, "skill_type", PV::Message(skill_type)).unwrap();
        set(&mut skill, "level", PV::U32(level)).unwrap();
        set(&mut data, "skill", PV::Message(skill)).unwrap();
    } else {
        let train_to_type = identifier(&data.descriptor(), "train_to_type", PV::U64(type_id)).unwrap();
        set(&mut data, "train_to_type", PV::Message(train_to_type)).unwrap();
    }
    set(&mut req, "milestone", PV::Message(data)).unwrap();
    req
}

/// A request whose only content is one `Identifier` field.
fn with_uuid(name: &str, field: &str, id: &[u8]) -> DynamicMessage {
    let mut req = request(name);
    attach(&mut req, field, id);
    req
}

/// The `data` of the first `milestones` entry in a `GetAllResponse`.
fn data_of(response: &DynamicMessage) -> DynamicMessage {
    response.get_field_by_name("milestones").unwrap().as_list().unwrap()[0]
        .as_message().unwrap().get_field_by_name("data").unwrap().as_message().cloned().unwrap()
}

/// A career-path category encoded the way the client writes `uuid.UUID(int=id)`.
async fn category(db: &crate::db::Db) -> Vec<u8> {
    let id: i64 = db.query_one("SELECT key::bigint AS id FROM sde_native.careerpaths ORDER BY key LIMIT 1", &[]).await.unwrap().get("id");
    let mut category = vec![0u8; 16];
    category[12..].copy_from_slice(&(id as u32).to_be_bytes());
    category
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

/// Valid prerequisite-free skills other than `skill`, for the five-slot plan.
fn free_skills(sde: &crate::sde::Sde, skill: i32) -> impl Iterator<Item = i32> + '_ {
    sde.types.iter()
        .filter(move |t| t.type_id != skill && validation::valid_skill(sde, t.type_id) && sde.skill_plans.requirements(t.type_id).is_empty())
        .map(|t| t.type_id)
}

async fn cleanup(db: &crate::db::Db, corp: i64, fixtures: &[fixture::Fixture]) {
    db.execute("DELETE FROM game.skill_plans WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=ANY($1)", &[&fixtures.iter().map(|f| f.character).collect::<Vec<_>>()]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    for f in fixtures { fixture::cleanup(db, *f).await.unwrap(); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn corporation_milestones_round_trip_membership_and_replacement() {
    let gw = crate::gateway::tests::gateway().await;
    let manager = fixture::create(&gw.db).await.unwrap();
    let member = fixture::create(&gw.db).await.unwrap();
    let outsider = fixture::create(&gw.db).await.unwrap();
    let corp = corporation(&gw.db, manager.character).await;
    assign(&gw.db, member.character, corp, 0, 0, 0, false).await;
    assign(&gw.db, manager.character, corp, MANAGER_ROLE, 0, 0, false).await;
    let _receiver = online(&gw, &member, corp);
    let mut stream = crate::gateway::tests::notice_stream(&gw.world, [7; 16], member.character, 16);
    let ctx = context(&gw, manager.account, manager.character).await;
    let member_ctx = context(&gw, member.account, member.character).await;
    let outsider_ctx = context(&gw, outsider.account, outsider.character).await;
    let category = category(&gw.db).await;
    // A non-skill type whose whole requirement tree is one prerequisite-free
    // skill, so the milestone's required set is exactly the plan's single step.
    let (ship, skill, level) = ctx.sde.types.iter()
        .filter(|t| t.category_id != Some(infinity_core::consts::CATEGORY_SKILL) && validation::target(ctx.sde, t.type_id, None))
        .find_map(|t| validation::required(ctx.sde, t.type_id, None).and_then(|required| {
            let (&skill, &level) = required.iter().next().filter(|_| required.len() == 1)?;
            (validation::valid_skill(ctx.sde, skill) && ctx.sde.skill_plans.requirements(skill).is_empty()).then_some((t.type_id, skill, level))
        }))
        .unwrap();
    let plan = uuid_bytes();
    plans::create(&gw.db, &plan, Owner::Corporation(corp), "Wing", "", &[skill], &[level], Some(&category)).await.unwrap();
    let (get_all, create, delete, describe) = (
        format!("{PREFIX}.GetAllRequest"), format!("{PREFIX}.CreateRequest"),
        format!("{PREFIX}.DeleteRequest"), format!("{PREFIX}.SetDescriptionRequest"));

    let created = handle(&ctx, &create, &create_request(&plan, ship as u64, None)).await.unwrap();
    let milestone = response_uuid(&created, "milestone");
    let (notice, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), format!("{PREFIX}.CreatedNotice"));
    assert!(matches!(notice.target_group.unwrap().target, Some(Target::Corporation(id)) if i64::from(id) == corp));
    assert_eq!((response_uuid(&payload, "identifier"), response_uuid(&payload, "skill_plan")), (milestone.clone(), plan.clone()));
    let attrs_field = payload.get_field_by_name("milestone").unwrap();
    let attrs = attrs_field.as_message().unwrap();
    assert_eq!(response_uuid(attrs, "skill_plan"), plan);
    assert_eq!(attrs.get_field_by_name("train_to_type").unwrap().as_message().unwrap().get_field_by_name("sequential").unwrap().as_u64(), Some(ship as u64));
    assert_eq!(attrs.get_field_by_name("description").unwrap().as_str(), Some(""));

    let listed = handle(&member_ctx, &get_all, &with_uuid(&get_all, "skill_plan", &plan)).await.unwrap();
    let milestones_field = listed.get_field_by_name("milestones").unwrap();
    let entries = milestones_field.as_list().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(data_of(&listed).has_field_by_name("train_to_type"));
    assert!(!entries[0].as_message().unwrap().has_field_by_name("progress"));
    refused(handle(&outsider_ctx, &get_all, &with_uuid(&get_all, "skill_plan", &plan)).await.unwrap_err(), FailureCode::NotFound);

    let mut update = with_uuid(&describe, "identifier", &milestone);
    set(&mut update, "description", PV::String("dock first".into())).unwrap();
    handle(&ctx, &describe, &update).await.unwrap();
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), format!("{PREFIX}.DescriptionSetNotice"));
    assert_eq!(response_uuid(&payload, "milestone"), milestone);
    assert_eq!(payload.get_field_by_name("description").unwrap().as_str(), Some("dock first"));
    let described = handle(&member_ctx, &get_all, &with_uuid(&get_all, "skill_plan", &plan)).await.unwrap();
    assert_eq!(data_of(&described).get_field_by_name("description").unwrap().as_str(), Some("dock first"));
    refused(handle(&member_ctx, &describe, &update).await.unwrap_err(), FailureCode::PreconditionFailed);

    handle(&ctx, &delete, &with_uuid(&delete, "milestone", &milestone)).await.unwrap();
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!((payload.descriptor().full_name(), response_uuid(&payload, "milestone")), (format!("{PREFIX}.DeletedNotice").as_str(), milestone.clone()));

    let skill_req = create_request(&plan, skill as u64, Some(1));
    let skill_id = response_uuid(&handle(&ctx, &create, &skill_req).await.unwrap(), "milestone");
    assert!(data_of(&handle(&member_ctx, &get_all, &with_uuid(&get_all, "skill_plan", &plan)).await.unwrap()).has_field_by_name("skill"));
    let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
    assert_eq!(payload.descriptor().full_name(), format!("{PREFIX}.CreatedNotice"));
    assert_eq!(response_uuid(&payload, "identifier"), skill_id);
    let milestone_field = payload.get_field_by_name("milestone").unwrap();
    let skill_field = milestone_field.as_message().unwrap().get_field_by_name("skill").unwrap();
    let skill_arm = skill_field.as_message().unwrap();
    assert_eq!(skill_arm.get_field_by_name("level").unwrap().as_u32(), Some(1));
    let mut neither = request(&create);
    let mut data = child(&neither.descriptor(), "milestone").unwrap();
    attach(&mut data, "skill_plan", &plan);
    set(&mut neither, "milestone", PV::Message(data)).unwrap();
    refused(handle(&ctx, &create, &neither).await.unwrap_err(), FailureCode::BadRequest);

    plans::delete(&gw.db, Owner::Corporation(corp), &plan).await.unwrap();
    assert!(plan_milestones::get(&gw.db, &skill_id).await.unwrap().is_none());
    assert!(gw.db.query_opt("SELECT 1 FROM game.skill_plan_milestone_progress WHERE milestone_id=$1", &[&skill_id]).await.unwrap().is_none());

    let full = uuid_bytes();
    let five: Vec<i32> = std::iter::once(skill).chain(free_skills(ctx.sde, skill)).take(5).collect();
    plans::create(&gw.db, &full, Owner::Corporation(corp), "full", "", &five, &[1; 5], Some(&category)).await.unwrap();
    let mut first = Vec::new();
    for id in &five {
        first.push(response_uuid(&handle(&ctx, &create, &create_request(&full, *id as u64, Some(1))).await.unwrap(), "milestone"));
    }
    refused(handle(&ctx, &create, &create_request(&full, five[0] as u64, Some(1))).await.unwrap_err(), FailureCode::PreconditionFailed);
    for expected in &first {
        let (_, payload) = crate::gateway::tests::decode_notice(&stream.rx.recv().await.unwrap());
        assert_eq!(payload.descriptor().full_name(), format!("{PREFIX}.CreatedNotice"));
        assert_eq!(response_uuid(&payload, "identifier"), *expected);
    }
    // The in-transaction quota refusal rolls back and publishes nothing.
    assert!(stream.rx.try_recv().is_err());
    handle(&ctx, &delete, &with_uuid(&delete, "milestone", &first[0])).await.unwrap();
    let replacement = response_uuid(&handle(&ctx, &create, &create_request(&full, five[0] as u64, Some(1))).await.unwrap(), "milestone");
    assert_ne!(replacement, first[0]);
    let remaining = handle(&ctx, &get_all, &with_uuid(&get_all, "skill_plan", &full)).await.unwrap();
    assert_eq!(remaining.get_field_by_name("milestones").unwrap().as_list().unwrap().len(), 5);
    refused(handle(&ctx, &get_all, &with_uuid(&get_all, "skill_plan", &[1, 2, 3])).await.unwrap_err(), FailureCode::BadRequest);
    assign(&gw.db, member.character, 1, 0, 0, 0, false).await;
    refused(handle(&member_ctx, &get_all, &with_uuid(&get_all, "skill_plan", &full)).await.unwrap_err(), FailureCode::NotFound);
    cleanup(&gw.db, corp, &[manager, member, outsider]).await;
}
