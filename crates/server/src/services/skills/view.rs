//! CharacterSkillEntry wire projection for real and effective skill state.
use super::{training, CallError, Client};
use crate::client::filetime_now;
use crate::sde::Sde;
use marshal::Value;

const CHARACTER_SKILL_ENTRY: &str =
    "characterskills.common.character_skill_entry.CharacterSkillEntry";

/// Encode a stable full-state skill entry. `None` means no persisted training.
pub(super) fn skill_info(
    sde: &Sde,
    type_id: i32,
    trained_level: Option<i32>,
    trained_points: Option<i64>,
    virtual_level: Option<i32>,
    now: i64,
) -> Value {
    Value::reduce(
        CHARACTER_SKILL_ENTRY,
        vec![
            Value::Int(type_id as i64),
            trained_level.map_or(Value::None, |level| Value::Int(level as i64)),
            trained_points.map_or(Value::None, Value::Int),
            Value::Float(training::skill_type(sde, type_id).rank),
            virtual_level.map_or(Value::None, |level| Value::Int(level as i64)),
        ],
        Some(Value::dict([("timeStamp", Value::Int(now))])),
    )
}

/// Encode a changed/deleted entry. Virtual-only changes use the client's
/// negative marker so it enters `GetCopyWithNewSkillPoints(None)`.
pub(super) fn incremental_skill_info(
    sde: &Sde,
    type_id: i32,
    trained_level: Option<i32>,
    trained_points: Option<i64>,
    virtual_level: Option<i32>,
    now: i64,
) -> Value {
    let (level, points) = match (trained_level, trained_points, virtual_level) {
        (None, None, Some(_)) => (Some(-1), Some(-1)),
        (None, None, None) => (Some(-1), Some(-1)),
        (level, points, _) => (level, points),
    };
    skill_info(sde, type_id, level, points, virtual_level, now)
}

pub(super) async fn skills_dict(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    let now = filetime_now();
    let snapshot = super::effective::snapshot_at(&c.db, &c.sde, char_id, now).await?;
    let training = training::in_training_sp(&c.db, char_id, now).await?;
    let mut entries = Vec::new();
    for (type_id, entry) in snapshot.iter() {
        let (trained_level, mut points) = entry.trained.as_ref()
            .map(|skill| (Some(skill.level), Some(skill.skill_points)))
            .unwrap_or((None, None));
        if let Some((training_type, trained_points)) = training {
            if training_type == type_id {
                points = points.map(|stored| stored.max(trained_points));
            }
        }
        entries.push((
            Value::Int(type_id as i64),
            skill_info(&c.sde, type_id, trained_level, points, entry.virtual_level, now),
        ));
    }
    Ok(Value::Dict(entries))
}

#[cfg(test)]
mod tests;
