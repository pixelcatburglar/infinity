//! Corporation skill-plan notice binding shared by the plan and milestone
//! adapters. The transaction commits the corporation and member snapshot, and
//! dispatch excludes the writing application and sessions that were not
//! members when the write committed. The nine definition payloads are built
//! here and published by those adapters.
use std::sync::Arc;
use anyhow::Result;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use crate::{dao::skills::plan_milestones::Milestone, services::{CallError, skill_plans::publication}};
use super::super::{context::HandlerContext, error::{FailureCode, GatewayError}, message::{named, set}, notices::{producer_error, GatewayNotice}, protocol::target_group::Target};
use super::wire;

/// Corporation writes need the transport-bound application. Reads do not call
/// this; a missing origin is a bad request, not an identity failure.
pub(super) fn require_write_origin(ctx: &HandlerContext<'_>) -> Result<(), GatewayError> {
    ctx.application_instance_uuid.is_some().then_some(()).ok_or_else(|| {
        GatewayError::refusal(FailureCode::BadRequest)
    })
}

/// A corporation-targeted notice bound at request time. The transaction
/// supplies the committed corporation and member snapshot at dispatch; the
/// writing application's origin is excluded and the snapshot becomes the
/// allowed audience. A read that never publishes ignores both.
pub(super) fn notice(ctx: &HandlerContext<'_>, payload: DynamicMessage) -> Arc<publication::Notice> {
    let origin = ctx.application_instance_uuid;
    Arc::new(move |audience: publication::Audience| {
        let corporation = u32::try_from(audience.corporation)
            .map_err(|_| CallError::Internal(anyhow::anyhow!("corporation notice target out of range")))?;
        Ok(GatewayNotice::new(Target::Corporation(corporation), &payload).map_err(producer_error)?
            .restrict_corporation(origin, audience.members))
    })
}

/// `CreatedNotice{identifier, skill_plan: Attributes}`.
pub(super) fn created_payload(id: &[u8], name: &str, description: &str, skills: &[i32], levels: &[i32], category: &[u8]) -> Result<DynamicMessage> {
    let mut notice = named("eve_public.corporation.skill.plan.CreatedNotice")?;
    put_uuid(&mut notice, "identifier", id)?;
    let attrs = wire::attributes(&notice.descriptor(), "skill_plan", name, description, skills, levels, category)?;
    set(&mut notice, "skill_plan", PV::Message(attrs))?;
    Ok(notice)
}

/// `DeletedNotice{skill_plan: Identifier}`.
pub(super) fn deleted_payload(id: &[u8]) -> Result<DynamicMessage> {
    let mut notice = named("eve_public.corporation.skill.plan.DeletedNotice")?;
    put_uuid(&mut notice, "skill_plan", id)?;
    Ok(notice)
}

/// `NameUpdatedNotice` or `DescriptionUpdatedNotice`: `{identifier, <field>}`.
pub(super) fn text_payload(kind: &str, id: &[u8], field: &str, value: &str) -> Result<DynamicMessage> {
    let mut notice = named(kind)?;
    put_uuid(&mut notice, "identifier", id)?;
    set(&mut notice, field, PV::String(value.into()))?;
    Ok(notice)
}

/// `SkillRequirementsUpdatedNotice{identifier, requirements}`.
pub(super) fn requirements_payload(id: &[u8], skills: &[i32], levels: &[i32]) -> Result<DynamicMessage> {
    let mut notice = named("eve_public.corporation.skill.plan.SkillRequirementsUpdatedNotice")?;
    put_uuid(&mut notice, "identifier", id)?;
    let requirements = wire::requirement_list(&notice.descriptor(), "requirements", skills, levels)?;
    set(&mut notice, "requirements", requirements)?;
    Ok(notice)
}

/// `CategoryUpdatedNotice{identifier, category}`.
pub(super) fn category_payload(id: &[u8], category: &[u8]) -> Result<DynamicMessage> {
    let mut notice = named("eve_public.corporation.skill.plan.CategoryUpdatedNotice")?;
    put_uuid(&mut notice, "identifier", id)?;
    let value = wire::uuid_message(&notice.descriptor(), "category", category.to_vec())?;
    set(&mut notice, "category", PV::Message(value))?;
    Ok(notice)
}

/// `id` into the `uuid` field of the `Identifier` message at `message.field`.
pub(super) fn put_uuid(message: &mut DynamicMessage, field: &str, id: &[u8]) -> Result<()> {
    let identifier = wire::uuid_message(&message.descriptor(), field, id.to_vec())?;
    set(message, field, PV::Message(identifier))
}

/// `CreatedNotice{identifier, milestone, skill_plan}`, `DeletedNotice{milestone}`
/// or `DescriptionSetNotice{milestone, description}`.
pub(super) fn payload(kind: &str, id: &[u8], description: Option<&str>, row: Option<&Milestone>) -> Result<DynamicMessage> {
    let mut notice = named(&format!("eve_public.corporation.skill.plan.milestone.{kind}"))?;
    put_uuid(&mut notice, if kind == "CreatedNotice" { "identifier" } else { "milestone" }, id)?;
    if let Some(row) = row {
        let milestone = wire::milestone_attributes(&notice.descriptor(), "milestone", row)?;
        set(&mut notice, "milestone", PV::Message(milestone))?;
        put_uuid(&mut notice, "skill_plan", &row.plan_id)?;
    }
    if let Some(description) = description {
        set(&mut notice, "description", PV::String(description.into()))?;
    }
    Ok(notice)
}
