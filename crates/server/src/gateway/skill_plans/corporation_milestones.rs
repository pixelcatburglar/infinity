//! Corporation milestone protocol for `corpMilestoneRequest.py`.
//! Members of the caller's live corporation read a plan's milestone list; writes
//! additionally need the skill-plan manager role bit, which the service enforces
//! inside its transaction. Definition changes publish a corporation-targeted
//! notice bound to the member snapshot committed with the write; per-viewer
//! progress is reconciled by the service and never sent on the wire.
use anyhow::Result;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{dao::skills::plan_milestones as dao, services::skill_plans::milestones};
use super::super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{child, msg_field, new_uuid, response_for, set, str_field}};
use super::{notices, wire};

fn invalid() -> GatewayError {
    GatewayError::refusal(FailureCode::BadRequest)
}

/// The `sequential` of a present `{Identifier}` field; only a positive id counts.
fn sequential(message: &DynamicMessage, field: &str) -> Option<i32> {
    msg_field(message, field)?.get_field_by_name("sequential")?.as_u64()
        .and_then(|id| i32::try_from(id).ok())
        .filter(|id| *id > 0)
}

/// A `GetAllResponse.milestones` entry: `identifier` plus `data`.
fn entry(response: &prost_reflect::MessageDescriptor, row: &dao::Milestone) -> Result<DynamicMessage> {
    let mut entry = child(response, "milestones")?;
    notices::put_uuid(&mut entry, "identifier", &row.milestone_id)?;
    let data = wire::milestone_attributes(&entry.descriptor(), "data", row)?;
    set(&mut entry, "data", PV::Message(data))?;
    Ok(entry)
}

/// The `milestone` oneof of the create `Attributes`: both arms set, or neither,
/// is a bad request shape.
fn target(data: &DynamicMessage) -> Result<(&'static str, i32, Option<i32>), GatewayError> {
    let (has_type, has_skill) = (data.has_field_by_name("train_to_type"), data.has_field_by_name("skill"));
    if has_type == has_skill { return Err(invalid()); }
    if has_type {
        return Ok(("type", sequential(data, "train_to_type").ok_or_else(invalid)?, None));
    }
    let skill = msg_field(data, "skill").ok_or_else(invalid)?;
    let level = skill.get_field_by_name("level").and_then(|v| v.as_u32())
        .and_then(|v| i32::try_from(v).ok()).ok_or_else(invalid)?;
    Ok(("skill", sequential(&skill, "skill_type").ok_or_else(invalid)?, Some(level)))
}

pub(in crate::gateway) async fn handle(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> std::result::Result<DynamicMessage, GatewayError> {
    let identity = ctx.require_character()?;
    let mut rsp = response_for(name).map_err(GatewayError::Internal)?;
    match name.rsplit('.').next().unwrap_or_default() {
        "GetAllRequest" => {
            let parent = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            let rows = milestones::corporation_list(identity, ctx.db, ctx.world, &parent).await.map_err(super::error)?;
            let list = rows.iter()
                .map(|row| entry(&rsp.descriptor(), row).map(PV::Message))
                .collect::<Result<Vec<_>>>().map_err(GatewayError::Internal)?;
            set(&mut rsp, "milestones", PV::List(list)).map_err(GatewayError::Internal)?;
        }
        "CreateRequest" => {
            let data = msg_field(req, "milestone").filter(|_| req.has_field_by_name("milestone")).ok_or_else(invalid)?;
            let parent = wire::uuid(&data, "skill_plan").ok_or_else(invalid)?;
            let (kind, type_id, level) = target(&data)?;
            let description = str_field(&data, "description");
            let id = new_uuid();
            let row = dao::Milestone { milestone_id: id.clone(), plan_id: parent.clone(), kind: kind.into(), type_id, level, description: description.clone() };
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::payload("CreatedNotice", &id, None, Some(&row)).map_err(GatewayError::Internal)?);
            milestones::corporation_create(identity, ctx.db, ctx.sde, ctx.world, &id, &parent, kind, type_id, level, &description, notice).await.map_err(super::error)?;
            notices::put_uuid(&mut rsp, "milestone", &id).map_err(GatewayError::Internal)?;
        }
        "DeleteRequest" => {
            let id = wire::uuid(req, "milestone").ok_or_else(invalid)?;
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::payload("DeletedNotice", &id, None, None).map_err(GatewayError::Internal)?);
            milestones::corporation_delete(identity, ctx.db, ctx.world, &id, notice).await.map_err(super::error)?;
        }
        "SetDescriptionRequest" => {
            let id = wire::uuid(req, "identifier").ok_or_else(invalid)?;
            let description = str_field(req, "description");
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::payload("DescriptionSetNotice", &id, Some(&description), None).map_err(GatewayError::Internal)?);
            milestones::corporation_set_description(identity, ctx.db, ctx.world, &id, &description, notice).await.map_err(super::error)?;
        }
        _ => return Err(invalid()),
    }
    Ok(rsp)
}

#[cfg(all(test, feature = "tests-gateway"))]
mod tests;
