//! Lua skill queue operations using the shared skill training service.
//! Mirrors skillHandler queue operations over game.skill_queue and SDE skills.
//! Training rules and scheduling remain owned by the training service.
use mlua::{Lua, Table, Value};
use crate::dao::skills::training as training_dao;
use crate::services::skills::training;
use super::{lifecycle, lua_error, mutate, positive, Resources};

fn queue_value(table: Table) -> mlua::Result<marshal::Value> {
    let mut entries = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::Integer(index) = key else { return Err(mlua::Error::runtime("invalid queue index")); };
        if index <= 0 { return Err(mlua::Error::runtime("invalid queue index")); }
        let Value::Table(record) = value else { return Err(mlua::Error::runtime("invalid queue record")); };
        let (mut type_id, mut level) = (None, None);
        for field in record.pairs::<Value, Value>() {
            let (key, value) = field?;
            let Value::String(key) = key else { return Err(mlua::Error::runtime("invalid queue field")); };
            let Value::Integer(value) = value else { return Err(mlua::Error::runtime("invalid queue value")); };
            if value <= 0 || i32::try_from(value).is_err() { return Err(mlua::Error::runtime("invalid queue value")); }
            match key.as_bytes().as_ref() {
                b"type_id" => type_id = Some(value),
                b"level" => level = Some(value),
                _ => return Err(mlua::Error::runtime("unknown queue field")),
            }
        }
        let (Some(type_id), Some(level)) = (type_id, level) else { return Err(mlua::Error::runtime("missing queue field")); };
        entries.push((index, type_id, level));
    }
    entries.sort_unstable_by_key(|entry| entry.0);
    let mut values = Vec::with_capacity(entries.len());
    for (position, (index, type_id, level)) in entries.into_iter().enumerate() {
        if i64::try_from(position).ok().and_then(|p| p.checked_add(1)) != Some(index) { return Err(mlua::Error::runtime("sparse queue")); }
        values.push((marshal::Value::Int(index - 1), marshal::Value::tuple([marshal::Value::Int(type_id), marshal::Value::Int(level)])));
    }
    Ok(marshal::Value::Dict(values))
}

pub(super) fn register(lua: &Lua, api: &Table, resources: &Resources) -> mlua::Result<()> {
    let r = resources.clone();
    api.set("skills", lua.create_async_function(move |lua, id: i64| {
        let r = r.clone(); async move {
            positive(id)?;
            let mut entries = mutate(async move {
                let (_state, _row) = lifecycle::locked(&r, id).await?;
                training_dao::current(&r.db, id).await.map_err(crate::services::db_err)
            }).await.map_err(lua_error)?;
            entries.sort_unstable_by_key(|entry| entry.type_id);
            let result = lua.create_table()?;
            for (index, entry) in entries.into_iter().enumerate() {
                let row = lua.create_table()?;
                row.set("type_id", entry.type_id)?;
                row.set("level", entry.level)?;
                row.set("skill_points", entry.skill_points)?;
                result.raw_set(index + 1, row)?;
            }
            Ok(result)
        }
    })?)?;
    let r = resources.clone();
    api.set("skill_queue", lua.create_async_function(move |lua, id: i64| {
        let r = r.clone(); async move {
            positive(id)?;
            let entries = mutate(async move {
                let (_state, row) = lifecycle::locked(&r, id).await?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                training::queue_list(&identity, &r.db).await
            }).await.map_err(lua_error)?;
            let result = lua.create_table()?;
            for (index, entry) in entries.into_iter().enumerate() {
                let row = lua.create_table()?;
                row.set("type_id", entry.type_id)?; row.set("level", entry.to_level)?;
                row.set("position", entry.position)?; row.set("start_sp", entry.start_sp)?; row.set("dest_sp", entry.dest_sp)?;
                row.set("start_time", entry.start_time)?; row.set("end_time", entry.end_time)?;
                result.raw_set(index + 1, row)?;
            }
            Ok(result)
        }
    })?)?;
    let r = resources.clone();
    api.set("save_skill_queue", lua.create_async_function(move |_, (id, queue): (i64, Table)| {
        let r = r.clone(); async move {
            positive(id)?;
            let queue = queue_value(queue)?;
            mutate(async move {
                let (_state, row) = lifecycle::locked(&r, id).await?;
                let identity = lifecycle::snapshot(&r, id, &row).await?;
                training::save_queue(&identity, &r.db, &r.sde, &queue, true, true, |n| n.dispatch(&r.world, None)).await
            }).await.map_err(lua_error)
        }
    })?)?;
    Ok(())
}
