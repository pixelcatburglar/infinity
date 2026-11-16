//! Personal milestone protocol for `personalMilestoneRequest.py`.
//! Uses personal plan definitions and viewer-specific snapshots from game tables.
//! Corporation milestones and completion notifications are not handled here.
use anyhow::Result;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{dao::skills::plan_milestones as dao, services::skill_plans::milestones};
use super::super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{child, msg_field, new_uuid, response_for, set, str_field}};
use super::wire;

fn invalid() -> GatewayError { GatewayError::refusal(FailureCode::BadRequest) }
fn numeric_id(message: &DynamicMessage, field: &str) -> Option<i32> {
    if !message.has_field_by_name(field) { return None; }
    let id = msg_field(message, field)?.get_field_by_name("sequential")?.as_u64()?;
    i32::try_from(id).ok().filter(|id| *id > 0)
}
fn encode(row: &dao::Milestone, response: &prost_reflect::MessageDescriptor) -> Result<DynamicMessage> {
    let mut entry = child(response, "milestones")?;
    let id = wire::uuid_message(&entry.descriptor(), "identifier", row.milestone_id.clone())?;
    set(&mut entry, "identifier", PV::Message(id))?;
    let data = wire::milestone_attributes(&entry.descriptor(), "data", row)?;
    set(&mut entry, "data", PV::Message(data))?;
    Ok(entry)
}
pub(in crate::gateway) async fn handle(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> std::result::Result<DynamicMessage, GatewayError> {
    let identity = ctx.require_character()?;
    let db = ctx.db;
    let mut rsp = response_for(name).map_err(GatewayError::Internal)?;
    match name.rsplit('.').next().unwrap_or_default() {
        "GetAllRequest" => {
            let parent = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            let rows = milestones::list(identity, db, ctx.world, &parent).await.map_err(super::error)?;
            let list = rows.iter().map(|row| encode(row, &rsp.descriptor()).map(PV::Message)).collect::<Result<Vec<_>>>().map_err(GatewayError::Internal)?;
            set(&mut rsp, "milestones", PV::List(list)).map_err(GatewayError::Internal)?;
        }
        "CreateRequest" => {
            if !req.has_field_by_name("milestone") { return Err(invalid()); }
            let data = msg_field(req, "milestone").ok_or_else(invalid)?;
            let parent = wire::uuid(&data, "skill_plan").ok_or_else(invalid)?;
            let has_type = data.has_field_by_name("train_to_type");
            let has_skill = data.has_field_by_name("skill");
            if has_type == has_skill { return Err(invalid()); }
            let (kind, type_id, level) = if has_type { ("type", numeric_id(&data, "train_to_type").ok_or_else(invalid)?, None) } else {
                let skill = msg_field(&data, "skill").ok_or_else(invalid)?;
                let id = numeric_id(&skill, "skill_type").ok_or_else(invalid)?;
                let level = skill.get_field_by_name("level").and_then(|v| v.as_u32()).and_then(|v| i32::try_from(v).ok()).ok_or_else(invalid)?;
                ("skill", id, Some(level))
            };
            let id = new_uuid();
            let description = str_field(&data, "description");
            milestones::create(identity, db, ctx.sde, ctx.world, &id, &parent, kind, type_id, level, &description).await.map_err(super::error)?;
            let mut identifier = child(&rsp.descriptor(), "milestone").map_err(GatewayError::Internal)?;
            set(&mut identifier, "uuid", PV::Bytes(Bytes::from(id))).map_err(GatewayError::Internal)?;
            set(&mut rsp, "milestone", PV::Message(identifier)).map_err(GatewayError::Internal)?;
        }
        method @ ("DeleteRequest" | "SetDescriptionRequest") => {
            let id = wire::uuid(req, if method == "DeleteRequest" { "milestone" } else { "identifier" }).ok_or_else(invalid)?;
            if method == "DeleteRequest" { milestones::delete(identity, db, &id).await.map_err(super::error)?; }
            else { milestones::set_description(identity, db, &id, &str_field(req, "description")).await.map_err(super::error)?; }
        }
        _ => return Err(invalid()),
    }
    Ok(rsp)
}

#[cfg(all(test, feature = "tests-gateway"))] mod tests;
