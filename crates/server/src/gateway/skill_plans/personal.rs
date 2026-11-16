//! Personal plan protocol for `requestsMessenger.py:create` and `requestsMessenger.py:get_all`.
//! Encodes game plan definitions; the shared service enforces ownership and transactions.
//! Corporation plans are not handled here.
use anyhow::Result;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use infinity_core::consts::UUID_LEN;
use crate::dao::skills::plans;
use super::super::{context::HandlerContext, message::{new_uuid, msg_field, str_field, response_for, set}};
use super::super::error::{GatewayError, FailureCode};
use crate::services::skill_plans::personal;
use super::wire;

/// `skill_plan.uuid`, including sixteen zero bytes. SetActive and GetActive use
/// that value for cancelled tracking; every other method rejects it.
fn plan_id(req: &DynamicMessage) -> Option<Vec<u8>> {
    if !req.has_field_by_name("skill_plan") { return None; }
    msg_field(req, "skill_plan")?.get_field_by_name("uuid")?.as_bytes().map(|b| b.to_vec())
}

pub(in crate::gateway) async fn skill_plan(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> std::result::Result<DynamicMessage, GatewayError> {
    let invalid = || GatewayError::refusal(FailureCode::BadRequest);
    let identity = ctx.require_character()?;
    let db = ctx.db;
    let mut rsp = response_for(name).map_err(GatewayError::Internal)?;
    let desc = rsp.descriptor();
    let method = name.rsplit('.').next().unwrap_or_default();
    let id = match plan_id(req) {
        Some(id) if id.len() != UUID_LEN => return Err(invalid()),
        id => id,
    };
    if method != "SetActiveRequest" && id.as_ref().is_some_and(|id| id.iter().all(|byte| *byte == 0)) { return Err(invalid()); }
    match method {
        "CreateRequest" => {
            let plan = msg_field(req, "skill_plan").filter(|_| req.has_field_by_name("skill_plan")).ok_or_else(invalid)?;
            let (skills, levels) = wire::requirements(&plan, "skill_requirements").ok_or_else(invalid)?;
            let name = str_field(&plan, "name");
            let description = str_field(&plan, "description");
            let new = new_uuid();
            personal::create(identity, db, ctx.sde, &new, &name, &description, &skills, &levels).await.map_err(super::error)?;
            let value = wire::uuid_message(&desc, "skill_plan", new).map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plan", PV::Message(value)).map_err(GatewayError::Internal)?;
        }
        "GetAllRequest" => {
            let rows = personal::all(identity, db).await.map_err(super::error)?;
            let ids = rows.into_iter().map(|id| wire::uuid_message(&desc, "skill_plans", id).map(PV::Message)).collect::<Result<Vec<_>>>().map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plans", PV::List(ids)).map_err(GatewayError::Internal)?;
        }
        "GetRequest" | "GetSharedRequest" => {
            let row = personal::get(identity, db, &id.ok_or_else(invalid)?, method == "GetSharedRequest").await.map_err(super::error)?;
            if let Some(row) = row {
                let value = wire::attributes(&desc, "skill_plan", &row.name, &row.description, &row.skill_ids, &row.levels, &[]).map_err(GatewayError::Internal)?;
                set(&mut rsp, "skill_plan", PV::Message(value)).map_err(GatewayError::Internal)?;
            }
        }
        "GetActiveRequest" => {
            let (tracking, row) = personal::active(identity, db).await.map_err(super::error)?;
            let mut plan = vec![0u8; UUID_LEN];
            if let Some(tracking) = tracking {
                if let Some(personal) = tracking.personal_plan_id {
                    if let Some(row) = row {
                        let value = wire::attributes(&desc, "skill_plan_info", &row.name, &row.description, &row.skill_ids, &row.levels, &[]).map_err(GatewayError::Internal)?;
                        set(&mut rsp, "skill_plan_info", PV::Message(value)).map_err(GatewayError::Internal)?;
                    }
                    plan = personal;
                } else if let Some(certified) = tracking.certified_plan_id {
                    let id = u32::try_from(certified).map_err(|error| GatewayError::Internal(error.into()))?;
                    plan[UUID_LEN - 4..].copy_from_slice(&id.to_be_bytes());
                }
            }
            let value = wire::uuid_message(&desc, "skill_plan", plan).map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plan", PV::Message(value)).map_err(GatewayError::Internal)?;
        }
        "SetActiveRequest" => personal::set_active(identity, db, ctx.sde, &id.ok_or_else(invalid)?).await.map_err(super::error)?,
        "DeleteRequest" => personal::delete(identity, db, &id.ok_or_else(invalid)?).await.map_err(super::error)?,
        "SetNameRequest" | "SetDescriptionRequest" => {
            let (column, field) = if method == "SetNameRequest" { ("name", plans::Field::Name) } else { ("description", plans::Field::Description) };
            personal::set_field(identity, db, &id.ok_or_else(invalid)?, field, &str_field(req, column)).await.map_err(super::error)?;
        }
        "SetSkillRequirementsRequest" => {
            let (skills, levels) = wire::requirements(req, "requirements").ok_or_else(invalid)?;
            personal::set_requirements(identity, db, ctx.sde, &id.ok_or_else(invalid)?, &skills, &levels).await.map_err(super::error)?;
        }
        _ => return Err(invalid()),
    }
    Ok(rsp)
}
#[cfg(all(test, feature = "tests-gateway"))]
mod tests;
