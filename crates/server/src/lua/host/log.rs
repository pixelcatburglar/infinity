//! Script-attributed diagnostic logging. Scripts supply messages; no client module
//! or database table is involved. Player-visible notifications are not logged here.

use mlua::{Lua, Result, Table};

pub(super) fn register(lua: &Lua, env: &Table, script: &str) -> Result<()> {
    let table = lua.create_table()?;
    let name = script.to_owned();
    table.set("info", lua.create_function(move |_, message: String| {
        tracing::info!(script = %name, "{message}");
        Ok(())
    })?)?;
    let name = script.to_owned();
    table.set("warn", lua.create_function(move |_, message: String| {
        tracing::warn!(script = %name, "{message}");
        Ok(())
    })?)?;
    let name = script.to_owned();
    table.set("debug", lua.create_function(move |_, message: String| {
        tracing::debug!(script = %name, "{message}");
        Ok(())
    })?)?;
    env.set("log", table)
}

#[cfg(all(test, feature = "tests-lua"))]
mod tests;
