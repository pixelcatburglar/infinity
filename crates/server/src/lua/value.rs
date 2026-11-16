//! Typed bridge to the client's Python marshal values; no implicit Lua table serialization.
//! Uses the marshal codec and the rowset formats consumed by the client.

use marshal::{DbRowDescriptor, DbType, Value};
use mlua::{Lua, MetaMethod, Table, UserData, UserDataMethods};

#[derive(Clone)]
pub struct WireValue(pub Value);

pub fn from_lua(value: mlua::Value) -> mlua::Result<Value> {
    if let mlua::Value::UserData(value) = value {
        if let Ok(value) = value.borrow::<WireValue>() {
            return Ok(value.0.clone());
        }
    }
    Err(mlua::Error::runtime("expected a wire value; use an eve constructor such as eve.int, eve.str, eve.list or eve.dict"))
}

fn validate_sequence(table: &Table) -> mlua::Result<usize> {
    let mut count = 0usize;
    let mut max = 0usize;
    for entry in table.clone().pairs::<mlua::Value, mlua::Value>() {
        let (key, _) = entry?;
        let mlua::Value::Integer(index) = key else { return Err(mlua::Error::runtime("expected a contiguous sequence with integer keys")); };
        let index = usize::try_from(index).map_err(|_| mlua::Error::runtime("sequence index must be positive"))?;
        if index == 0 { return Err(mlua::Error::runtime("sequence index must be positive")); }
        count += 1;
        max = max.max(index);
    }
    if max != count { return Err(mlua::Error::runtime("sequence must not contain holes")); }
    Ok(count)
}

pub(super) fn sequence(table: Table) -> mlua::Result<Vec<Value>> {
    validate_sequence(&table)?;
    table.sequence_values::<mlua::Value>().map(|v| from_lua(v?)).collect()
}

fn fields(table: Table) -> mlua::Result<Vec<(String, Value)>> {
    table.pairs::<String, mlua::Value>().map(|entry| {
        let (key, value) = entry?;
        Ok((key, from_lua(value)?))
    }).collect()
}

impl UserData for WireValue {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("int", |_, this, ()| this.0.as_int().ok_or_else(|| mlua::Error::runtime("wire value is not an integer")));
        methods.add_method("float", |_, this, ()| this.0.as_float().ok_or_else(|| mlua::Error::runtime("wire value is not a number")));
        methods.add_method("text", |_, this, ()| this.0.as_text().map(|s| s.into_owned()).ok_or_else(|| mlua::Error::runtime("wire value is not text")));
        methods.add_method("bool", |_, this, ()| match this.0 { Value::Bool(v) => Ok(v), _ => Err(mlua::Error::runtime("wire value is not a bool")) });
        methods.add_method("is_none", |_, this, ()| Ok(this.0.is_none()));
        methods.add_method("len", |_, this, ()| match &this.0 {
            Value::Tuple(v) | Value::List(v) => Ok(v.len()),
            Value::Dict(v) => Ok(v.len()),
            Value::Bytes(v) => Ok(v.len()),
            Value::Unicode(v) => Ok(v.chars().count()),
            _ => Err(mlua::Error::runtime("wire value has no length")),
        });
        methods.add_method("items", |lua, this, ()| {
            let items = this.0.items().ok_or_else(|| mlua::Error::runtime("wire value is not a tuple or list"))?;
            let result = lua.create_table()?;
            for (i, item) in items.iter().enumerate() { result.set(i + 1, WireValue(item.clone()))?; }
            Ok(result)
        });
        methods.add_method("get", |_, this, key: mlua::Value| {
            let value = match (&this.0, key) {
                (Value::Dict(entries), mlua::Value::String(key)) => entries.iter().find(|(k, _)| k.as_bytes().is_some_and(|b| b == key.as_bytes().as_ref())).map(|(_, v)| v),
                (Value::Tuple(items) | Value::List(items), mlua::Value::Integer(index)) if index > 0 => items.get(index as usize - 1),
                _ => None,
            };
            Ok(value.cloned().map(WireValue))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(crate::client::short(&this.0)));
    }
}

pub fn register(lua: &Lua, env: &Table) -> mlua::Result<()> {
    let eve = lua.create_table()?;
    eve.set("none", WireValue(Value::None))?;
    eve.set("int", lua.create_function(|_, v: i64| Ok(WireValue(Value::Int(v))))?)?;
    eve.set("float", lua.create_function(|_, v: f64| Ok(WireValue(Value::Float(v))))?)?;
    eve.set("bool", lua.create_function(|_, v: bool| Ok(WireValue(Value::Bool(v))))?)?;
    eve.set("str", lua.create_function(|_, v: mlua::LuaString| Ok(WireValue(Value::Bytes(v.as_bytes().to_vec()))))?)?;
    eve.set("unicode", lua.create_function(|_, v: String| Ok(WireValue(Value::Unicode(v))))?)?;
    eve.set("tuple", lua.create_function(|_, v: Table| Ok(WireValue(Value::Tuple(sequence(v)?))))?)?;
    eve.set("list", lua.create_function(|_, v: Table| Ok(WireValue(Value::List(sequence(v)?))))?)?;
    eve.set("dict", lua.create_function(|_, v: Table| Ok(WireValue(Value::dict(fields(v)?))))?)?;
    eve.set("keyval", lua.create_function(|_, v: Table| Ok(WireValue(crate::services::keyval(fields(v)?))))?)?;
    eve.set("instance", lua.create_function(|_, (class, v): (String, Table)| Ok(WireValue(Value::instance(&class, fields(v)?))))?)?;
    eve.set("reduce", lua.create_function(|_, (callable, args, state): (String, Table, Option<mlua::Value>)| Ok(WireValue(Value::reduce(&callable, sequence(args)?, state.map(from_lua).transpose()?))))?)?;
    eve.set("rowset", lua.create_function(|_, (header, rows): (Table, Table)| {
        validate_sequence(&header)?;
        validate_sequence(&rows)?;
        let header: Vec<String> = header.sequence_values().collect::<mlua::Result<_>>()?;
        let lines = rows.sequence_values::<Table>().map(|r| sequence(r?)).collect::<mlua::Result<Vec<_>>>()?;
        if lines.iter().any(|line| line.len() != header.len()) { return Err(mlua::Error::runtime("row length does not match header")); }
        Ok(WireValue(crate::services::rowset(&header.iter().map(String::as_str).collect::<Vec<_>>(), lines)))
    })?)?;
    eve.set("descriptor", lua.create_function(|_, columns: Table| {
        validate_sequence(&columns)?;
        let columns = columns.sequence_values::<Table>().map(|c| {
            let c = c?;
            if validate_sequence(&c)? != 2 { return Err(mlua::Error::runtime("descriptor column must contain name and type")); }
            let name: String = c.get(1)?;
            let ty: String = c.get(2)?;
            let ty = match ty.as_str() {
                "i1" => DbType::I1, "i2" => DbType::I2, "i4" => DbType::I4, "i8" => DbType::I8,
                "ui1" => DbType::UI1, "ui2" => DbType::UI2, "ui4" => DbType::UI4, "ui8" => DbType::UI8,
                "r4" => DbType::R4, "r8" => DbType::R8, "cy" => DbType::Cy, "bool" => DbType::Bool,
                "filetime" => DbType::FileTime, "bytes" => DbType::Bytes, "str" => DbType::Str, "wstr" => DbType::WStr,
                "timestamp" => DbType::DbTimestamp,
                _ => return Err(mlua::Error::runtime(format!("unknown descriptor type {ty}"))),
            };
            Ok((name, ty))
        }).collect::<mlua::Result<Vec<_>>>()?;
        Ok(WireValue(DbRowDescriptor::new(columns).to_value()))
    })?)?;
    eve.set("crowset", lua.create_function(|_, (descriptor, rows): (mlua::Value, Table)| {
        let descriptor = from_lua(descriptor)?;
        let descriptor = DbRowDescriptor::from_value(&descriptor).ok_or_else(|| mlua::Error::runtime("expected eve.descriptor"))?;
        validate_sequence(&rows)?;
        let rows = rows.sequence_values::<Table>().map(|row| {
            let values = sequence(row?)?;
            if values.len() != descriptor.columns.len() { return Err(mlua::Error::runtime("row length does not match descriptor")); }
            Ok(descriptor.row(&values))
        }).collect::<mlua::Result<Vec<_>>>()?;
        Ok(WireValue(crate::services::crowset(&descriptor, rows)))
    })?)?;
    env.set("eve", eve)
}

#[cfg(all(test, feature = "tests-lua"))]
mod tests;
