//! Personal plans for `requestsMessenger.py:create` and `requestsMessenger.py:get_all`.
//! Uses the skills plan DAO and descriptors; guarded ownership and activation
//! are enforced by the gateway and DAO transaction boundaries.
use anyhow::Result;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage, Value as PV};
use crate::dao::skills::plans;
use super::super::{context::HandlerContext, message::{UUID_LEN, new_uuid, msg_field, str_field, identifier, child, set, response_for}};
use super::super::error::{GatewayError, FailureCode};

/// `skill_plan.uuid` of a request.
fn plan_id(req: &DynamicMessage) -> Option<Vec<u8>> {
    msg_field(req, "skill_plan")?.get_field_by_name("uuid")?.as_bytes().map(|b| b.to_vec())
}

/// (skill type IDs, levels) of repeated `eve_public.skill.plan.SkillRequirement`.
fn requirements(m: &DynamicMessage, field: &str) -> Option<(Vec<i32>, Vec<i32>)> {
    let (mut skills, mut levels) = (vec![], vec![]);
    if let Some(list) = m.get_field_by_name(field) {
        for r in list.as_list()?.iter().filter_map(|v| v.as_message()) {
            let skill = msg_field(r, "skill_type").and_then(|t| t.get_field_by_name("sequential")?.as_u64()).filter(|id| *id > 0 && *id <= i32::MAX as u64)?;
            let level = r.get_field_by_name("level").and_then(|v| v.as_u32()).filter(|level| (1..=5).contains(level))?;
            skills.push(skill as i32);
            levels.push(level as i32);
        }
    }
    Some((skills, levels))
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


pub(in crate::gateway) async fn skill_plan(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> std::result::Result<DynamicMessage, GatewayError> {
    let invalid = || GatewayError::refusal(FailureCode::BadRequest, crate::messages::gateway::INVALID_REQUEST);
    let me = ctx.require_character()?.char_id;
    let db = ctx.db;
    let mut rsp = response_for(name).map_err(GatewayError::Internal)?;
    let desc = rsp.descriptor();
    let method = name.rsplit('.').next().unwrap_or_default();
    let id = match plan_id(req) {
        Some(id) if id.len() != UUID_LEN => return Err(GatewayError::refusal(FailureCode::PreconditionFailed, crate::messages::gateway::INVALID_REQUEST)),
        id => id,
    };
    match method {
        "CreateRequest" => {
            let plan = msg_field(req, "skill_plan").ok_or_else(invalid)?;
            let (skills, levels) = requirements(&plan, "skill_requirements").ok_or_else(|| GatewayError::refusal(FailureCode::PreconditionFailed, crate::messages::gateway::INVALID_REQUEST))?;
            let new = new_uuid();
            plans::create(db, &new, me, &str_field(&plan, "name"), &str_field(&plan, "description"), &skills, &levels).await?;
            let value = plan_identifier(&desc, "skill_plan", new).map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plan", PV::Message(value)).map_err(GatewayError::Internal)?;
        }
        "GetAllRequest" => {
            let rows = plans::all(db, me).await?;
            let ids = rows.into_iter().map(|id| plan_identifier(&desc, "skill_plans", id).map(PV::Message)).collect::<Result<Vec<_>>>().map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plans", PV::List(ids)).map_err(GatewayError::Internal)?;
        }
        "GetRequest" | "GetSharedRequest" => {
            let owner = if method == "GetRequest" { Some(me) } else { None };
            let row = plans::get(db, &id.ok_or_else(invalid)?, owner).await?;
            let row = row.ok_or_else(|| GatewayError::refusal(FailureCode::NotFound, crate::messages::gateway::INVALID_REQUEST))?;
            let value = plan_attributes(&desc, "skill_plan", &row).map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plan", PV::Message(value)).map_err(GatewayError::Internal)?;
        }
        "GetActiveRequest" => {
            let row = plans::active(db, me).await?;
            let plan = row.as_ref().map(|r| r.plan_id.clone()).unwrap_or_else(|| vec![0u8; UUID_LEN]);
            let value = plan_identifier(&desc, "skill_plan", plan).map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plan", PV::Message(value)).map_err(GatewayError::Internal)?;
            if let Some(row) = row {
                let value = plan_attributes(&desc, "skill_plan_info", &row).map_err(GatewayError::Internal)?;
                set(&mut rsp, "skill_plan_info", PV::Message(value)).map_err(GatewayError::Internal)?;
            }
        }
        "SetActiveRequest" => {
            let plan_id = id.ok_or_else(invalid)?;
            let activated = db.transaction(|tx, _: &mut crate::db::outbox::Outbox| {
                let plan_id = plan_id.clone();
                Box::pin(async move { plans::activate(tx, me, &plan_id).await })
            }, |value, _| async move { Ok(value) }).await?;
            if !activated {
                return Err(GatewayError::refusal(FailureCode::NotFound, crate::messages::gateway::INVALID_REQUEST));
            }
        }
        "DeleteRequest" => {
            let plan_id = id.ok_or_else(invalid)?;
            if plans::delete(db, me, &plan_id).await? != 1 { return Err(GatewayError::refusal(FailureCode::NotFound, crate::messages::gateway::INVALID_REQUEST)); }
        }
        "SetNameRequest" | "SetDescriptionRequest" => {
            let (field, column) = if method == "SetNameRequest" { (plans::Field::Name, "name") } else { (plans::Field::Description, "description") };
            let plan_id = id.ok_or_else(invalid)?;
            if plans::set_field(db, me, &plan_id, field, &str_field(req, column)).await? != 1 { return Err(GatewayError::refusal(FailureCode::NotFound, crate::messages::gateway::INVALID_REQUEST)); }
        }
        "SetSkillRequirementsRequest" => {
            let (skills, levels) = requirements(req, "requirements").ok_or_else(|| GatewayError::refusal(FailureCode::PreconditionFailed, crate::messages::gateway::INVALID_REQUEST))?;
            let plan_id = id.ok_or_else(invalid)?;
            if plans::set_requirements(db, me, &plan_id, &skills, &levels).await? != 1 { return Err(GatewayError::refusal(FailureCode::NotFound, crate::messages::gateway::INVALID_REQUEST)); }
        }
        _ => return Err(invalid()),
    }
    Ok(rsp)
}
#[cfg(test)]
mod tests;
