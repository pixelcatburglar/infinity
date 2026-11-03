//! Career definitions and progress for `request_messenger.py:get_goals_definition_request`
//! and `request_messenger.py:get_goals_and_rewards_progress_request`.
//! Uses fixed career roots and descriptors, not World or DAO data; progress
//! is zero and these roots are not full career content.
use anyhow::Result;
use bytes::Bytes;
use prost_reflect::{DynamicMessage, ReflectMessage, Value as PV};
use super::message::{UUID_LEN, UUID_VERSION_INDEX, UUID_VERSION_V4, UUID_VARIANT_INDEX, UUID_VARIANT_RFC4122, child, set, response_for};

/// eve_public.career.goal.Attributes.Career for the four career paths.
const CAREERS: [i32; 4] = [1, 2, 3, 4];

/// Career goal ids: one top-level goal per career path (careergoals/client/goal_data_controller.py
/// needs `career_path_goal_definition` for every career).
fn career_goal_uuid(career: i32) -> Bytes {
    let mut id = [0u8; UUID_LEN];
    id[UUID_VERSION_INDEX] = UUID_VERSION_V4;
    id[UUID_VARIANT_INDEX] = UUID_VARIANT_RFC4122;
    id[UUID_LEN - 1] = career as u8;
    Bytes::copy_from_slice(&id)
}

fn goal_identifier(entry: &DynamicMessage, career: i32) -> Result<DynamicMessage> {
    let mut goal = child(&entry.descriptor(), "goal")?;
    set(&mut goal, "uuid", PV::Bytes(career_goal_uuid(career)))?;
    Ok(goal)
}

pub(super) fn career_goal_definitions() -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.career.goal.api.GetDefinitionsRequest")?;
    let mut goals = Vec::new();
    for career in CAREERS {
        let mut entry = child(&rsp.descriptor(), "goals")?;
        let goal = goal_identifier(&entry, career)?;
        set(&mut entry, "goal", PV::Message(goal))?;
        let mut attrs = child(&entry.descriptor(), "attributes")?;
        for msg in ["name", "description"] {
            let id = child(&attrs.descriptor(), msg)?;
            set(&mut attrs, msg, PV::Message(id))?;
        }
        set(&mut attrs, "target", PV::U32(1))?;
        set(&mut attrs, "career", PV::EnumNumber(career))?;
        set(&mut entry, "attributes", PV::Message(attrs))?;
        goals.push(PV::Message(entry));
    }
    set(&mut rsp, "goals", PV::List(goals))?;
    Ok(rsp)
}

pub(super) fn career_goal_states() -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.career.goal.api.GetAllRequest")?;
    let mut goals = Vec::new();
    for career in CAREERS {
        let mut entry = child(&rsp.descriptor(), "goals")?;
        let goal = goal_identifier(&entry, career)?;
        set(&mut entry, "goal", PV::Message(goal))?;
        set(&mut entry, "progress", PV::U32(0))?;
        set(&mut entry, "not_completed", PV::Bool(true))?;
        goals.push(PV::Message(entry));
    }
    set(&mut rsp, "goals", PV::List(goals))?;
    Ok(rsp)
}

#[cfg(test)] mod tests;
