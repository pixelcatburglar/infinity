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
    let skill = owner_ctx.sde.types.iter().find(|t| crate::services::skill_plans::validation::valid_skill(owner_ctx.sde, t.type_id) && owner_ctx.sde.skill_plans.requirements(t.type_id).is_empty()).unwrap().type_id;

    let mut create = request(CREATE);
    let mut plan = child(&create.descriptor(), "skill_plan").unwrap();
    set(&mut plan, "name", PV::String("Initial name".into())).unwrap();
    set(&mut plan, "description", PV::String("Initial description".into())).unwrap();
    set_requirements(&mut plan, "skill_requirements", &[(skill as u64, 1), (skill as u64, 2)]);
    set(&mut create, "skill_plan", PV::Message(plan)).unwrap();
    let created = super::skill_plan(&owner_ctx, CREATE, &create).await.unwrap();
    let plan_id = response_uuid(&created, "skill_plan");
    assert_eq!(plan_id.len(), 16);

    for (field, accepted, rejected) in [
        ("name", "中".repeat(50), "中".repeat(51)),
        ("name", "🚀".repeat(25), format!("{}中", "🚀".repeat(25))),
        ("name", "中".repeat(50), format!("{}🚀", "中".repeat(49))),
        ("description", "中".repeat(1000), "中".repeat(1001)),
        ("description", "🚀".repeat(500), format!("{}中", "🚀".repeat(500))),
        ("description", "中".repeat(1000), format!("{}🚀", "中".repeat(999))),
        ("description", "a\u{301}".repeat(500), format!("{}a", "a\u{301}".repeat(500))),
    ] {
        let mut boundary = request(CREATE);
        let mut attributes = child(&boundary.descriptor(), "skill_plan").unwrap();
        set(&mut attributes, "name", PV::String("Boundary".into())).unwrap();
        set(&mut attributes, field, PV::String(accepted.clone())).unwrap();
        set(&mut boundary, "skill_plan", PV::Message(attributes.clone())).unwrap();
        let created = super::skill_plan(&owner_ctx, CREATE, &boundary).await.unwrap();
        let id = response_uuid(&created, "skill_plan");
        let read = super::skill_plan(&owner_ctx, GET, &uuid_request(GET, &id)).await.unwrap();
        let read_plan = read.get_field_by_name("skill_plan").unwrap();
        assert_eq!(read_plan.as_message().unwrap().get_field_by_name(field).unwrap().as_str(), Some(accepted.as_str()));
        set(&mut attributes, field, PV::String(rejected)).unwrap();
        set(&mut boundary, "skill_plan", PV::Message(attributes)).unwrap();
        refused(super::skill_plan(&owner_ctx, CREATE, &boundary).await.unwrap_err(), FailureCode::PreconditionFailed);
        let original = super::skill_plan(&owner_ctx, GET, &uuid_request(GET, &plan_id)).await.unwrap();
        let original_plan = original.get_field_by_name("skill_plan").unwrap();
        let original_plan = original_plan.as_message().unwrap();
        assert_eq!(original_plan.get_field_by_name("name").unwrap().as_str(), Some("Initial name"));
        assert_eq!(original_plan.get_field_by_name("description").unwrap().as_str(), Some("Initial description"));
        super::skill_plan(&owner_ctx, DELETE, &uuid_request(DELETE, &id)).await.unwrap();
    }

    let row = gw.db.query_one("SELECT character_id, name, description, skill_ids, levels FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
    assert_eq!(row.get::<_, i64>("character_id"), owner.character);
    assert_eq!(row.get::<_, String>("name"), "Initial name");
    assert_eq!(row.get::<_, Vec<i32>>("skill_ids"), vec![skill, skill]);
    assert_eq!(row.get::<_, Vec<i32>>("levels"), vec![1, 2]);

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
    set_requirements(&mut requirements, "requirements", &[(skill as u64, 1)]);
    super::skill_plan(&owner_ctx, SET_REQUIREMENTS, &requirements).await.unwrap();
    let row = gw.db.query_one("SELECT name, description, skill_ids, levels FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
    assert_eq!(row.get::<_, String>("name"), "Renamed");
    assert_eq!(row.get::<_, String>("description"), "Redescribed");
    assert_eq!(row.get::<_, Vec<i32>>("skill_ids"), vec![skill]);
    assert_eq!(row.get::<_, Vec<i32>>("levels"), vec![1]);
    for (method, field, accepted, rejected) in [
        (SET_NAME, "name", "中".repeat(50), "中".repeat(51)),
        (SET_NAME, "name", "🚀".repeat(25), format!("{}中", "🚀".repeat(25))),
        (SET_NAME, "name", "中".repeat(50), format!("{}🚀", "中".repeat(49))),
        (SET_DESCRIPTION, "description", "中".repeat(1000), "中".repeat(1001)),
        (SET_DESCRIPTION, "description", "🚀".repeat(500), format!("{}中", "🚀".repeat(500))),
        (SET_DESCRIPTION, "description", "中".repeat(1000), format!("{}🚀", "中".repeat(999))),
        (SET_DESCRIPTION, "description", "a\u{301}".repeat(500), format!("{}a", "a\u{301}".repeat(500))),
    ] {
        let mut request = uuid_request(method, &plan_id);
        set(&mut request, field, PV::String(accepted.clone())).unwrap();
        super::skill_plan(&owner_ctx, method, &request).await.unwrap();
        let before = gw.db.query_one("SELECT name, description FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
        set(&mut request, field, PV::String(rejected)).unwrap();
        refused(super::skill_plan(&owner_ctx, method, &request).await.unwrap_err(), FailureCode::PreconditionFailed);
        let read = super::skill_plan(&owner_ctx, GET, &uuid_request(GET, &plan_id)).await.unwrap();
        let read_plan = read.get_field_by_name("skill_plan").unwrap();
        assert_eq!(read_plan.as_message().unwrap().get_field_by_name(field).unwrap().as_str(), Some(accepted.as_str()));
        let after = gw.db.query_one("SELECT name, description FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap();
        for column in ["name", "description"] {
            assert_eq!(after.get::<_, String>(column), before.get::<_, String>(column));
        }
    }
    set_requirements(&mut requirements, "requirements", &[(skill as u64, 0), (skill as u64, 1)]);
    super::skill_plan(&owner_ctx, SET_REQUIREMENTS, &requirements).await.unwrap();
    for (level, code) in [(6, FailureCode::PreconditionFailed), (u32::MAX, FailureCode::BadRequest)] {
        set_requirements(&mut requirements, "requirements", &[(skill as u64, level)]);
        refused(super::skill_plan(&owner_ctx, SET_REQUIREMENTS, &requirements).await.unwrap_err(), code);
    }

    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &plan_id)).await.unwrap();
    assert!(gw.db.query_opt("SELECT 1 FROM game.skill_plan_tracking WHERE character_id=$1 AND personal_plan_id=$2", &[&owner.character, &plan_id]).await.unwrap().is_some());
    let active = super::skill_plan(&owner_ctx, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), plan_id);

    let zero = vec![0_u8; 16];
    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &zero)).await.unwrap();
    let active = super::skill_plan(&owner_ctx, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), zero);
    assert!(gw.db.query_opt("SELECT 1 FROM game.skill_plan_tracking WHERE character_id=$1", &[&owner.character]).await.unwrap().is_none());
    let certified: u32 = gw.db.query_one("SELECT key FROM sde_native.skillplans ORDER BY row_no LIMIT 1", &[]).await.unwrap().get::<_, String>("key").parse().unwrap();
    let mut certified_uuid = vec![0; 16];
    certified_uuid[12..].copy_from_slice(&certified.to_be_bytes());
    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &certified_uuid)).await.unwrap();
    let active = super::skill_plan(&owner_ctx, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), certified_uuid);
    assert!(!active.has_field_by_name("skill_plan_info"));
    let reopened = context(&gw, owner.account, owner.character).await;
    let active = super::skill_plan(&reopened, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), certified_uuid);
    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &zero)).await.unwrap();
    let reopened = context(&gw, owner.account, owner.character).await;
    let active = super::skill_plan(&reopened, GET_ACTIVE, &request(GET_ACTIVE)).await.unwrap();
    assert_eq!(response_uuid(&active, "skill_plan"), zero);
    let absent = super::skill_plan(&owner_ctx, SHARED, &uuid_request(SHARED, &[9; 16])).await.unwrap();
    assert!(!absent.has_field_by_name("skill_plan"));

    refused(super::skill_plan(&other_ctx, SET_NAME, &uuid_request(SET_NAME, &plan_id)).await.unwrap_err(), FailureCode::NotFound);
    refused(super::skill_plan(&owner_ctx, DELETE, &uuid_request(DELETE, &[9; 16])).await.unwrap_err(), FailureCode::NotFound);
    refused(super::skill_plan(&other_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &plan_id)).await.unwrap_err(), FailureCode::NotFound);
    super::skill_plan(&owner_ctx, SET_ACTIVE, &uuid_request(SET_ACTIVE, &plan_id)).await.unwrap();
    assert_eq!(super::skill_plan(&owner_ctx, DELETE, &uuid_request(DELETE, &plan_id)).await.unwrap().descriptor().full_name(), "eve_public.character.skill.plan.DeleteResponse");
    assert!(gw.db.query_opt("SELECT 1 FROM game.skill_plans WHERE plan_id=$1", &[&plan_id]).await.unwrap().is_none());
    assert!(gw.db.query_opt("SELECT 1 FROM game.skill_plan_tracking WHERE character_id=$1", &[&owner.character]).await.unwrap().is_none());

    let malformed = uuid_request(GET, &[1, 2, 3]);
    refused(super::skill_plan(&owner_ctx, GET, &malformed).await.unwrap_err(), FailureCode::BadRequest);
    for (level, code) in [(6, FailureCode::PreconditionFailed), (u32::MAX, FailureCode::BadRequest)] {
        let mut bad = request(CREATE);
        let mut bad_plan = child(&bad.descriptor(), "skill_plan").unwrap();
        set_requirements(&mut bad_plan, "skill_requirements", &[(skill as u64, level)]);
        set(&mut bad, "skill_plan", PV::Message(bad_plan)).unwrap();
        refused(super::skill_plan(&owner_ctx, CREATE, &bad).await.unwrap_err(), code);
    }

    fixture::cleanup(&gw.db, owner).await.unwrap();
    fixture::cleanup(&gw.db, other).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn concurrent_personal_creates_obey_ten_plan_cap() {
    let gw = crate::gateway::tests::gateway().await;
    let owner = fixture::create(&gw.db).await.unwrap();
    let ctx = context(&gw, owner.account, owner.character).await;
    let mut create = request(CREATE);
    let mut plan = child(&create.descriptor(), "skill_plan").unwrap();
    set(&mut plan, "name", PV::String("Concurrent plan".into())).unwrap();
    set(&mut create, "skill_plan", PV::Message(plan)).unwrap();
    for _ in 0..9 {
        super::skill_plan(&ctx, CREATE, &create).await.unwrap();
    }
    let (first, second) = tokio::join!(
        super::skill_plan(&ctx, CREATE, &create),
        super::skill_plan(&ctx, CREATE, &create),
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    refused(first.err().or_else(|| second.err()).unwrap(), FailureCode::PreconditionFailed);
    let rows = crate::dao::skills::plans::all(&gw.db, owner.character).await.unwrap();
    assert_eq!(rows.len(), 10);
    for row in rows {
        crate::dao::skills::plans::delete(&gw.db, owner.character, &row).await.unwrap();
    }
    fixture::cleanup(&gw.db, owner).await.unwrap();
}
