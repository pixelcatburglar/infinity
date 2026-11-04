use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};

use crate::gateway::{Caller, Gateway};
use crate::gateway::context::HandlerContext;
use crate::gateway::error::{FailureCode, GatewayError};
use crate::gateway::message::{child, identifier, message, set};
use crate::services::tests::fixture;

const CREATE: &str = "eve_public.character.skill.plan.CreateRequest";
const GET: &str = "eve_public.character.skill.plan.GetRequest";
const SHARED: &str = "eve_public.character.skill.plan.GetSharedRequest";
const SET_NAME: &str = "eve_public.character.skill.plan.SetNameRequest";
const SET_DESCRIPTION: &str = "eve_public.character.skill.plan.SetDescriptionRequest";
const SET_REQUIREMENTS: &str = "eve_public.character.skill.plan.SetSkillRequirementsRequest";
const SET_ACTIVE: &str = "eve_public.character.skill.plan.SetActiveRequest";
const GET_ACTIVE: &str = "eve_public.character.skill.plan.GetActiveRequest";
const DELETE: &str = "eve_public.character.skill.plan.DeleteRequest";

fn request(name: &str) -> DynamicMessage {
    DynamicMessage::new(message(name).unwrap())
}

fn uuid_request(name: &str, id: &[u8]) -> DynamicMessage {
    let mut req = request(name);
    let mut plan = child(&req.descriptor(), "skill_plan").unwrap();
    set(&mut plan, "uuid", PV::Bytes(Bytes::from(id.to_vec()))).unwrap();
    set(&mut req, "skill_plan", PV::Message(plan)).unwrap();
    req
}


fn set_requirements(req: &mut DynamicMessage, field: &str, values: &[(u64, u32)]) {
    let mut entries = Vec::new();
    for &(skill, level) in values {
        let mut item = child(&req.descriptor(), field).unwrap();
        let skill_type = identifier(&item.descriptor(), "skill_type", PV::U64(skill)).unwrap();
        set(&mut item, "skill_type", PV::Message(skill_type)).unwrap();
        set(&mut item, "level", PV::U32(level)).unwrap();
        entries.push(PV::Message(item));
    }
    set(req, field, PV::List(entries)).unwrap();
}

fn response_uuid(response: &DynamicMessage, field: &str) -> Vec<u8> {
    response.get_field_by_name(field).unwrap().as_message().unwrap()
        .get_field_by_name("uuid").unwrap().as_bytes().unwrap().to_vec()
}

fn refused(error: GatewayError, code: FailureCode) {
    assert!(matches!(error, GatewayError::Refused { code: actual, .. } if std::mem::discriminant(&actual) == std::mem::discriminant(&code)));
}


async fn context<'a>(gw: &'a Gateway, account: i64, character: i64) -> HandlerContext<'a> {
    HandlerContext::for_request(gw, Caller { user_id: Some(account), character_id: Some(character) }).await.unwrap()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn personal_skill_plan_handler_round_trip_and_guards() {
    let gw = crate::gateway::tests::gateway().await;
    let owner = fixture::create(&gw.db).await.unwrap();
    let other = fixture::create(&gw.db).await.unwrap();
    let owner_ctx = context(&gw, owner.account, owner.character).await;
    let other_ctx = context(&gw, other.account, other.character).await;

    let mut create = request(CREATE);
    let mut plan = child(&create.descriptor(), "skill_plan").unwrap();
    set(&mut plan, "name", PV::String("Initial name".into())).unwrap();
    set(&mut plan, "description", PV::String("Initial description".into())).unwrap();
    set_requirements(&mut plan, "skill_requirements", &[(3300, 2), (3301, 5)]);
    set(&mut create, "skill_plan", PV::Message(plan)).unwrap();
    let created = super::skill_plan(&owner_ctx, CREATE, &create).await.unwrap();
    let plan_id = response_uuid(&created, "skill_plan");
    assert_eq!(plan_id.len(), 16);

    let row = gw.db.query_one("SELECT character_id, name, description, skill_ids, levels, active FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("character_id"), owner.character);
    assert_eq!(row.get::<_, String>("name"), "Initial name");
    assert_eq!(row.get::<_, Vec<i32>>("skill_ids"), vec![3300, 3301]);
    assert_eq!(row.get::<_, Vec<i32>>("levels"), vec![2, 5]);
    assert!(!row.get::<_, bool>("active"));

    let got = super::skill_plan(&owner_ctx, GET, &uuid_request(GET, &plan_id)).await.unwrap();
    let got_plan_field = got.get_field_by_name("skill_plan").unwrap();
    let got_plan = got_plan_field.as_message().unwrap();
    assert_eq!(got_plan.get_field_by_name("name").unwrap().as_str(), Some("Initial name"));
    let shared = super::skill_plan(&other_ctx, SHARED, &uuid_request(SHARED, &plan_id)).await.unwrap();
    let shared_plan_field = shared.get_field_by_name("skill_plan").unwrap();
    let shared_plan = shared_plan_field.as_message().unwrap();
    assert_eq!(shared_plan.get_field_by_name("name").unwrap().as_str(), Some("Initial name"));

    for (name, field, value) in [(SET_NAME, "name", "Renamed"), (SET_DESCRIPTION, "description", "Redescribed")] {
        let mut req = uuid_request(name, &plan_id);
        set(&mut req, field, PV::String(value.into())).unwrap();
        super::skill_plan(&owner_ctx, name, &req).await.unwrap();
    }
    let mut requirements = uuid_request(SET_REQUIREMENTS, &plan_id);
    set_requirements(&mut requirements, "requirements", &[(3302, 4)]);
    super::skill_plan(&owner_ctx, SET_REQUIREMENTS, &requirements).await.unwrap();
    let row = gw.db.query_one("SELECT name, description, skill_ids, levels FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
    assert_eq!(row.get::<_, String>("name"), "Renamed");
    assert_eq!(row.get::<_, String>("description"), "Redescribed");
    assert_eq!(row.get::<_, Vec<i32>>("skill_ids"), vec![3302]);
    assert_eq!(row.get::<_, Vec<i32>>("levels"), vec![4]);

    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &plan_id)).await.unwrap();
    assert!(gw.db.query_one("SELECT active FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap().get::<_, bool>("active"));
    let active = super::skill_plan(&owner_ctx, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), plan_id);

    let zero = vec![0_u8; 16];
    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &zero)).await.unwrap();
    let active = super::skill_plan(&owner_ctx, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), zero);
    assert!(!gw.db.query_one("SELECT active FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap().get::<_, bool>("active"));

    refused(super::skill_plan(&other_ctx, SET_NAME, &uuid_request(SET_NAME, &plan_id)).await.unwrap_err(), FailureCode::NotFound);
    refused(super::skill_plan(&owner_ctx, DELETE, &uuid_request(DELETE, &[9; 16])).await.unwrap_err(), FailureCode::NotFound);
    refused(super::skill_plan(&other_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &plan_id)).await.unwrap_err(), FailureCode::NotFound);
    assert_eq!(super::skill_plan(&owner_ctx, DELETE, &uuid_request(DELETE, &plan_id)).await.unwrap().descriptor().full_name(), "eve_public.character.skill.plan.DeleteResponse");
    assert!(gw.db.query_opt("SELECT 1 FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap().is_none());

    let malformed = uuid_request(GET, &[1, 2, 3]);
    refused(super::skill_plan(&owner_ctx, GET, &malformed).await.unwrap_err(), FailureCode::PreconditionFailed);
    for level in [0, 6] {
        let mut bad = request(CREATE);
        let mut bad_plan = child(&bad.descriptor(), "skill_plan").unwrap();
        set_requirements(&mut bad_plan, "skill_requirements", &[(3300, level)]);
        set(&mut bad, "skill_plan", PV::Message(bad_plan)).unwrap();
        refused(super::skill_plan(&owner_ctx, CREATE, &bad).await.unwrap_err(), FailureCode::PreconditionFailed);
    }

    fixture::cleanup(&gw.db, owner).await.unwrap();
    fixture::cleanup(&gw.db, other).await.unwrap();
}
