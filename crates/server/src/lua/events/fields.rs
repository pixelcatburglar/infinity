//! Typed Lua projections shared by historical fact events.
use mlua::{Lua, Table, Value};
use super::resources::FactContext;

pub(super) trait LuaField { fn value(&self, lua: &Lua) -> mlua::Result<Value>; }
pub(super) fn set_field<T: LuaField>(lua: &Lua, table: &Table, name: &str, value: &T) -> mlua::Result<()> { table.set(name, value.value(lua)?) }
macro_rules! integer { ($($t:ty),*) => { $(impl LuaField for $t { fn value(&self, _: &Lua) -> mlua::Result<Value> { Ok(Value::Integer(*self as i64)) } })* }; }
integer!(i64, i32, i16, u8);
impl LuaField for u64 { fn value(&self, lua: &Lua) -> mlua::Result<Value> { Ok(Value::String(lua.create_string(self.to_string())?)) } }
impl LuaField for bool { fn value(&self, _: &Lua) -> mlua::Result<Value> { Ok(Value::Boolean(*self)) } }
impl LuaField for f64 { fn value(&self, _: &Lua) -> mlua::Result<Value> { Ok(Value::Number(*self)) } }
impl LuaField for String { fn value(&self, lua: &Lua) -> mlua::Result<Value> { Ok(Value::String(lua.create_string(self)?)) } }
impl LuaField for crate::isk::IskAmount { fn value(&self, lua: &Lua) -> mlua::Result<Value> { mlua::IntoLua::into_lua(*self, lua) } }
impl<T: LuaField> LuaField for Option<T> { fn value(&self, lua: &Lua) -> mlua::Result<Value> { match self { Some(v) => v.value(lua), None => Ok(Value::Nil) } } }
impl<T: LuaField> LuaField for Vec<T> { fn value(&self, lua: &Lua) -> mlua::Result<Value> { let t = lua.create_table()?; for (i, v) in self.iter().enumerate() { t.set(i + 1, v.value(lua)?)?; } Ok(Value::Table(t)) } }
impl<const N: usize> LuaField for [u8; N] { fn value(&self, lua: &Lua) -> mlua::Result<Value> { let t = lua.create_table()?; for (i, v) in self.iter().enumerate() { t.set(i + 1, *v)?; } Ok(Value::Table(t)) } }
impl LuaField for FactContext {
    fn value(&self, lua: &Lua) -> mlua::Result<Value> {
        let t = lua.create_table()?;
        t.set("corporation_id", self.corporation_id)?; t.set("alliance_id", self.alliance_id)?;
        t.set("faction_id", self.faction_id)?; t.set("ship_id", self.ship_id)?;
        t.set("ship_type_id", self.ship_type_id)?; t.set("system_id", self.system_id)?;
        t.set("station_id", self.station_id)?; t.set("origin", self.origin.as_str())?;
        Ok(Value::Table(t))
    }
}
