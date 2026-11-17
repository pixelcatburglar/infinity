//! Corporation plan protocol for `corpSkillPlanRequest.py`.
//! Members of the caller's live corporation read summaries and attributes; writes
//! additionally need the skill-plan manager role bit, which the service enforces
//! inside its transaction. Definition changes publish a corporation-targeted
//! notice bound to the member snapshot committed with the write.
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{dao::skills::plans, services::skill_plans::corporation};
use super::super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{child, msg_field, new_uuid, response_for, set, str_field}};
use super::{notices, wire};

fn invalid() -> GatewayError { GatewayError::refusal(FailureCode::BadRequest) }

pub(in crate::gateway) async fn handle(ctx: &HandlerContext<'_>, name: &str, req: &DynamicMessage) -> std::result::Result<DynamicMessage, GatewayError> {
    let identity = ctx.require_character()?;
    let mut rsp = response_for(name).map_err(GatewayError::Internal)?;
    let desc = rsp.descriptor();
    match name.rsplit('.').next().unwrap_or_default() {
        "GetAllRequest" => {
            let rows = corporation::all(identity, ctx.db).await.map_err(super::error)?;
            let mut entries = vec![];
            for row in &rows {
                let mut entry = child(&desc, "skill_plans").map_err(GatewayError::Internal)?;
                let identifier = wire::uuid_message(&entry.descriptor(), "identifier", row.plan_id.clone()).map_err(GatewayError::Internal)?;
                set(&mut entry, "identifier", PV::Message(identifier)).map_err(GatewayError::Internal)?;
                let summary = wire::summary(&entry.descriptor(), "skill_plan", row).map_err(GatewayError::Internal)?;
                set(&mut entry, "skill_plan", PV::Message(summary)).map_err(GatewayError::Internal)?;
                entries.push(PV::Message(entry));
            }
            set(&mut rsp, "skill_plans", PV::List(entries)).map_err(GatewayError::Internal)?;
        }
        method @ ("GetRequest" | "GetSharedRequest") => {
            let id = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            let row = corporation::get(identity, ctx.db, &id, method == "GetSharedRequest").await.map_err(super::error)?;
            if let Some(row) = row {
                let category = row.category.as_deref().ok_or_else(|| GatewayError::Internal(anyhow::anyhow!("corporation plan without a category")))?;
                let attrs = wire::attributes(&desc, "skill_plan", &row.name, &row.description, &row.skill_ids, &row.levels, category).map_err(GatewayError::Internal)?;
                set(&mut rsp, "skill_plan", PV::Message(attrs)).map_err(GatewayError::Internal)?;
            }
        }
        "CreateRequest" => {
            let plan = msg_field(req, "skill_plan").filter(|_| req.has_field_by_name("skill_plan")).ok_or_else(invalid)?;
            let (skills, levels) = wire::requirements(&plan, "skill_requirements").ok_or_else(invalid)?;
            let category = wire::category(&plan, "category").ok_or_else(invalid)?;
            let name = str_field(&plan, "name");
            let description = str_field(&plan, "description");
            let id = new_uuid();
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::created_payload(&id, &name, &description, &skills, &levels, &category).map_err(GatewayError::Internal)?);
            corporation::create(identity, ctx.db, ctx.sde, ctx.world, &id, &name, &description, &skills, &levels, &category, notice).await.map_err(super::error)?;
            let identifier = wire::uuid_message(&desc, "skill_plan", id).map_err(GatewayError::Internal)?;
            set(&mut rsp, "skill_plan", PV::Message(identifier)).map_err(GatewayError::Internal)?;
        }
        "DeleteRequest" => {
            let id = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::deleted_payload(&id).map_err(GatewayError::Internal)?);
            corporation::delete(identity, ctx.db, ctx.world, &id, notice).await.map_err(super::error)?;
        }
        method @ ("SetNameRequest" | "SetDescriptionRequest") => {
            let id = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            let (field, column, notice_name) = if method == "SetNameRequest" {
                (plans::Field::Name, "name", "eve_public.corporation.skill.plan.NameUpdatedNotice")
            } else {
                (plans::Field::Description, "description", "eve_public.corporation.skill.plan.DescriptionUpdatedNotice")
            };
            let value = str_field(req, column);
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::text_payload(notice_name, &id, column, &value).map_err(GatewayError::Internal)?);
            corporation::set_field(identity, ctx.db, ctx.world, &id, field, &value, notice).await.map_err(super::error)?;
        }
        "SetSkillRequirementsRequest" => {
            let id = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            let (skills, levels) = wire::requirements(req, "requirements").ok_or_else(invalid)?;
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::requirements_payload(&id, &skills, &levels).map_err(GatewayError::Internal)?);
            corporation::set_requirements(identity, ctx.db, ctx.sde, ctx.world, &id, &skills, &levels, notice).await.map_err(super::error)?;
        }
        "SetCategoryRequest" => {
            let id = wire::uuid(req, "skill_plan").ok_or_else(invalid)?;
            let category = wire::category(req, "category").ok_or_else(invalid)?;
            notices::require_write_origin(ctx)?;
            let notice = notices::notice(ctx, notices::category_payload(&id, &category).map_err(GatewayError::Internal)?);
            corporation::set_category(identity, ctx.db, ctx.sde, ctx.world, &id, &category, notice).await.map_err(super::error)?;
        }
        _ => return Err(invalid()),
    }
    Ok(rsp)
}

#[cfg(all(test, feature = "tests-gateway"))]
mod tests;
