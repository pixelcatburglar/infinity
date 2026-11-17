//! Shared descriptor codec for personal and corporation skill-plan adapters:
//! UUID identifiers, ordered skill requirements, categories and the
//! Summary/Attributes split, including milestone attributes. The client reads
//! order-preserving repeated `eve_public.skill.plan.SkillRequirement` entries;
//! a Summary carries no requirement arrays.
use crate::dao::skills::{plan_milestones::Milestone, plans::Summary};
use anyhow::Result;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage, Value as PV};
use infinity_core::consts::UUID_LEN;
use super::super::message::{child, identifier, msg_field, set};

/// The `uuid` bytes of a present `{Identifier}` field; only a 16-byte value with
/// at least one nonzero byte is a plan or milestone reference.
pub(super) fn uuid(m: &DynamicMessage, field: &str) -> Option<Vec<u8>> {
    if !m.has_field_by_name(field) { return None; }
    let id = msg_field(m, field)?.get_field_by_name("uuid")?.as_bytes()?.to_vec();
    (id.len() == UUID_LEN && id.iter().any(|b| *b != 0)).then_some(id)
}

/// The 16-byte `category.Identifier.uuid` of a present category field. The
/// all-zero value is the client's "no category" entry and stays opaque here.
pub(super) fn category(m: &DynamicMessage, field: &str) -> Option<Vec<u8>> {
    if !m.has_field_by_name(field) { return None; }
    let id = msg_field(m, field)?.get_field_by_name("uuid")?.as_bytes()?.to_vec();
    (id.len() == UUID_LEN).then_some(id)
}

/// Ordered (skill type IDs, levels) of a repeated `SkillRequirement` field.
pub(super) fn requirements(m: &DynamicMessage, field: &str) -> Option<(Vec<i32>, Vec<i32>)> {
    let (mut skills, mut levels) = (vec![], vec![]);
    if let Some(list) = m.get_field_by_name(field) {
        for value in list.as_list()? {
            let r = value.as_message()?;
            if !r.has_field_by_name("skill_type") { return None; }
            let skill = msg_field(r, "skill_type").and_then(|t| t.get_field_by_name("sequential")?.as_u64()).and_then(|id| i32::try_from(id).ok()).filter(|id| *id > 0)?;
            let level = r.get_field_by_name("level").and_then(|v| v.as_u32()).and_then(|level| i32::try_from(level).ok())?;
            skills.push(skill);
            levels.push(level);
        }
    }
    Some((skills, levels))
}

/// An `{Identifier}` message carrying `id` in its `uuid` field.
pub(super) fn uuid_message(desc: &MessageDescriptor, field: &str, id: Vec<u8>) -> Result<DynamicMessage> {
    let mut m = child(desc, field)?;
    set(&mut m, "uuid", PV::Bytes(Bytes::from(id)))?;
    Ok(m)
}

/// Encoded repeated `SkillRequirement` list for `desc.field`, preserving order.
pub(super) fn requirement_list(desc: &MessageDescriptor, field: &str, skills: &[i32], levels: &[i32]) -> Result<PV> {
    let mut entries = vec![];
    for (&skill, &level) in skills.iter().zip(levels) {
        let mut r = child(desc, field)?;
        let skill_type = identifier(&r.descriptor(), "skill_type", PV::U64(skill as u64))?;
        set(&mut r, "skill_type", PV::Message(skill_type))?;
        set(&mut r, "level", PV::U32(level as u32))?;
        entries.push(PV::Message(r));
    }
    Ok(PV::List(entries))
}

/// `Attributes` for `desc.field`: name, description and the ordered skill
/// requirements. An empty `category` is omitted, so personal attributes keep
/// their shape; corporation attributes always carry the 16-byte identifier,
/// including sixteen zero bytes for no category.
pub(super) fn attributes(desc: &MessageDescriptor, field: &str, name: &str, description: &str, skills: &[i32], levels: &[i32], category: &[u8]) -> Result<DynamicMessage> {
    let mut attrs = child(desc, field)?;
    set(&mut attrs, "name", PV::String(name.into()))?;
    set(&mut attrs, "description", PV::String(description.into()))?;
    let requirements = requirement_list(&attrs.descriptor(), "skill_requirements", skills, levels)?;
    set(&mut attrs, "skill_requirements", requirements)?;
    if !category.is_empty() {
        let category = uuid_message(&attrs.descriptor(), "category", category.to_vec())?;
        set(&mut attrs, "category", PV::Message(category))?;
    }
    Ok(attrs)
}

/// Corporation `Summary` for `desc.field`. Unlike `attributes` the list shape
/// carries no requirement arrays; the client lazy-loads them through Get.
pub(super) fn summary(desc: &MessageDescriptor, field: &str, row: &Summary) -> Result<DynamicMessage> {
    let mut summary = child(desc, field)?;
    set(&mut summary, "name", PV::String(row.name.clone()))?;
    set(&mut summary, "description", PV::String(row.description.clone()))?;
    let category = uuid_message(&summary.descriptor(), "category", row.category.clone())?;
    set(&mut summary, "category", PV::Message(category))?;
    Ok(summary)
}

/// Milestone `Attributes` for `desc.field`: the parent identifier, exactly one
/// `milestone` oneof arm and the description. Skill targets carry
/// `skill{skill_type, level}`; type targets carry `train_to_type`.
pub(super) fn milestone_attributes(desc: &MessageDescriptor, field: &str, row: &Milestone) -> Result<DynamicMessage> {
    let mut data = child(desc, field)?;
    let plan = uuid_message(&data.descriptor(), "skill_plan", row.plan_id.clone())?;
    set(&mut data, "skill_plan", PV::Message(plan))?;
    if let Some(level) = row.level {
        let mut skill = child(&data.descriptor(), "skill")?;
        let skill_type = identifier(&skill.descriptor(), "skill_type", PV::U64(row.type_id as u64))?;
        set(&mut skill, "skill_type", PV::Message(skill_type))?;
        set(&mut skill, "level", PV::U32(level as u32))?;
        set(&mut data, "skill", PV::Message(skill))?;
    } else {
        let train_to_type = identifier(&data.descriptor(), "train_to_type", PV::U64(row.type_id as u64))?;
        set(&mut data, "train_to_type", PV::Message(train_to_type))?;
    }
    set(&mut data, "description", PV::String(row.description.clone()))?;
    Ok(data)
}
