//! Personal plans for `requestsMessenger.py:create` and `requestsMessenger.py:get_all`.
//! Uses the skills plan DAO and descriptors, not World; unsupported methods on
//! the legacy broad route fall through to existing default handling.
use anyhow::{Context, Result};
use bytes::Bytes;
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage, Value as PV};
use crate::dao::skills::plans;
use super::super::{Gateway, Caller, resources::db, message::{UUID_LEN, new_uuid, msg_field, str_field, identifier, child, set, response_for}};

/// `skill_plan.uuid` of a request.
fn plan_id(req: &DynamicMessage) -> Option<Vec<u8>> {
    msg_field(req, "skill_plan")?.get_field_by_name("uuid")?.as_bytes().map(|b| b.to_vec())
}

/// (skill type IDs, levels) of repeated `eve_public.skill.plan.SkillRequirement`.
fn requirements(m: &DynamicMessage, field: &str) -> (Vec<i32>, Vec<i32>) {
    let (mut skills, mut levels) = (vec![], vec![]);
    if let Some(list) = m.get_field_by_name(field) {
        for r in list.as_list().unwrap_or_default().iter().filter_map(|v| v.as_message()) {
            let skill = msg_field(r, "skill_type").and_then(|t| t.get_field_by_name("sequential")?.as_u64()).unwrap_or(0);
            let level = r.get_field_by_name("level").and_then(|v| v.as_u32()).unwrap_or(0);
            skills.push(skill as i32);
            levels.push(level as i32);
        }
    }
    (skills, levels)
}

/// An `eve_public.skill.plan.Attributes` for `desc.field`.
fn plan_attributes(desc: &MessageDescriptor, field: &str, row: &plans::Plan) -> Result<DynamicMessage> {
    let mut attrs = child(desc, field)?;
    set(&mut attrs, "name", PV::String(row.name.clone()))?;
    set(&mut attrs, "description", PV::String(row.description.clone()))?;
    let (skills, levels) = (&row.skill_ids, &row.levels);
    let mut reqs = vec![];
    for (skill, level) in skills.iter().zip(levels) {
        let mut r = child(&attrs.descriptor(), "skill_requirements")?;
        let id = identifier(&r.descriptor(), "skill_type", PV::U64(*skill as u64))?;
        set(&mut r, "skill_type", PV::Message(id))?;
        set(&mut r, "level", PV::U32(*level as u32))?;
        reqs.push(PV::Message(r));
    }
    set(&mut attrs, "skill_requirements", PV::List(reqs))?;
    Ok(attrs)
}

fn plan_identifier(desc: &MessageDescriptor, field: &str, id: Vec<u8>) -> Result<DynamicMessage> {
    let mut m = child(desc, field)?;
    set(&mut m, "uuid", PV::Bytes(Bytes::from(id)))?;
    Ok(m)
}


pub(in crate::gateway) async fn skill_plan(gw: &Gateway, caller: &Caller, name: &str, req: &DynamicMessage) -> Result<Option<DynamicMessage>> {
    let me = caller.character_id.context("no character")?;
    let db = db(gw)?;
    let mut rsp = response_for(name)?;
    let desc = rsp.descriptor();
    let method = name.rsplit('.').next().unwrap_or_default();
    let id = plan_id(req);
    match method {
        "CreateRequest" => {
            let plan = msg_field(req, "skill_plan").context("no skill_plan")?;
            let (skills, levels) = requirements(&plan, "skill_requirements");
            let new = new_uuid();
            plans::create(db, &new, me, &str_field(&plan, "name"), &str_field(&plan, "description"), &skills, &levels).await?;
            set(&mut rsp, "skill_plan", PV::Message(plan_identifier(&desc, "skill_plan", new)?))?;
        }
        "GetAllRequest" => {
            let rows = plans::all(db, me).await?;
            let ids = rows.into_iter().map(|id| plan_identifier(&desc, "skill_plans", id).map(PV::Message)).collect::<Result<Vec<_>>>()?;
            set(&mut rsp, "skill_plans", PV::List(ids))?;
        }
        // Shared plans are looked up by id regardless of owner.
        "GetRequest" | "GetSharedRequest" => {
            let owner = if method == "GetRequest" { Some(me) } else { None };
            let row = plans::get(db, &id.context("no skill_plan")?, owner).await?;
            if let Some(row) = row {
                set(&mut rsp, "skill_plan", PV::Message(plan_attributes(&desc, "skill_plan", &row)?))?;
            }
        }
        "GetActiveRequest" => {
            let row = plans::active(db, me).await?;
            // No active plan: the all-zero uuid.
            let plan = row.as_ref().map(|r| r.plan_id.clone()).unwrap_or_else(|| vec![0u8; UUID_LEN]);
            set(&mut rsp, "skill_plan", PV::Message(plan_identifier(&desc, "skill_plan", plan)?))?;
            if let Some(row) = row {
                set(&mut rsp, "skill_plan_info", PV::Message(plan_attributes(&desc, "skill_plan_info", &row)?))?;
            }
        }
        "SetActiveRequest" => {
            plans::set_active(db, me, &id.unwrap_or_default()).await?;
        }
        "DeleteRequest" => {
            plans::delete(db, me, &id.context("no skill_plan")?).await?;
        }
        "SetNameRequest" | "SetDescriptionRequest" => {
            let (field, column) = if method == "SetNameRequest" { (plans::Field::Name, "name") } else { (plans::Field::Description, "description") };
            plans::set_field(db, me, &id.context("no skill_plan")?, field, &str_field(req, column)).await?;
        }
        "SetSkillRequirementsRequest" => {
            let (skills, levels) = requirements(req, "requirements");
            plans::set_requirements(db, me, &id.context("no skill_plan")?, &skills, &levels).await?;
        }
        _ => return Ok(None),
    }
    Ok(Some(rsp))
}
