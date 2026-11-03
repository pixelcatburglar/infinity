//! Real character skill wire entries for characterskills/common/character_skill_entry.py.
//! Reads game.skills and the active game.skill_queue interval with startup SDE data.
//! Virtual skill grants are not represented here.
use super::{db_err, training, CallError, Client};
use crate::client::filetime_now;
use crate::dao::skills::training as training_dao;
use crate::sde::Sde;
use marshal::Value;

pub(super) fn skill_info(sde: &Sde, type_id: i32, level: i32, points: i64, now: i64) -> Value {
    Value::reduce(
        "characterskills.common.character_skill_entry.CharacterSkillEntry",
        vec![
            Value::Int(type_id as i64),
            Value::Int(level as i64),
            Value::Int(points),
            Value::Float(training::skill_type(sde, type_id).rank),
            Value::None,
        ],
        Some(Value::dict([("timeStamp", Value::Int(now))])),
    )
}

pub(super) async fn skills_dict(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    let now = filetime_now();
    let training = training::in_training_sp(&c.db, char_id, now).await?;
    let rows = training_dao::current(&c.db, char_id).await.map_err(db_err)?;
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let (type_id, level, mut sp) = (r.type_id, r.level, r.skill_points);
                if let Some((t, trained)) = training {
                    if t == type_id { sp = sp.max(trained); }
                }
                (Value::Int(type_id as i64), skill_info(&c.sde, type_id, level, sp, now))
            })
            .collect(),
    ))
}

#[cfg(test)] mod tests;
