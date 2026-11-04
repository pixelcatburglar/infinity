use super::*;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lua-data-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn write(&self, name: &str, value: impl AsRef<[u8]>) { std::fs::write(self.0.join(name), value).unwrap(); }
}
impl Drop for Fixture {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); }
}

#[test]
fn nested_values_and_fresh_loads() -> mlua::Result<()> {
    let dir = Fixture::new();
    std::fs::create_dir(dir.0.join("nested")).unwrap();
    dir.write("nested/doc.json", r#"{"integer":1,"float":1.0,"items":[true,"hello",{"n":2}]}"#);
    let lua = Lua::new();
    let env = environment(&lua, &dir.0, LuaConfig::default().data_file_limit_bytes)?;
    lua.load(r#"
        local a = data.load('nested.doc')
        assert(math.type(a.integer) == 'integer' and math.type(a.float) == 'float')
        assert(a.items[1] == true and a.items[2] == 'hello' and a.items[3].n == 2 and a.items[0] == nil)
        a.items[3].n = 9
        assert(data.load('nested.doc').items[3].n == 2)
    "#).set_environment(env.clone()).exec()?;
    dir.write("nested/doc.json", "{\"integer\":7}");
    assert_eq!(lua.load("return data.load('nested.doc').integer").set_environment(env).eval::<i64>()?, 7);
    Ok(())
}

#[test]
fn names_parse_null_depth_and_size_errors_include_name() -> mlua::Result<()> {
    let dir = Fixture::new();
    dir.write("bad.json", "{\n  \"x\": ]\n}");
    dir.write("null.json", "{\"x\":[null]}");
    dir.write("exact.json", "[1]");
    dir.write("over.json", "[10]");
    let lua = Lua::new();
    let env = environment(&lua, &dir.0, 3)?;
    let data: Table = env.get("data")?;
    let load: Function = data.get("load")?;
    assert!(load.call::<mlua::Value>("exact").is_ok());
    let error = load.call::<mlua::Value>("over").unwrap_err().to_string();
    assert!(error.contains("over") && error.contains("exceeds 3 bytes"), "{error}");
    let env = environment(&lua, &dir.0, LuaConfig::default().data_file_limit_bytes)?;
    let load: Function = env.get::<Table>("data")?.get("load")?;
    for name in ["", ".bad", "a..b", "../outside", "missing", "null"] {
        let error = load.call::<mlua::Value>(name).unwrap_err().to_string();
        assert!(error.contains(name), "{error}");
    }
    let error = load.call::<mlua::Value>("null").unwrap_err().to_string();
    assert!(error.contains("invalid null"), "{error}");
    let error = load.call::<mlua::Value>("bad").unwrap_err().to_string();
    assert!(error.contains("bad") && error.contains("line 2") && error.contains("column"), "{error}");
    let depth = crate::lua::json::MAX_DEPTH;
    dir.write("depth.json", format!("{}1{}", "[".repeat(depth), "]".repeat(depth)));
    assert!(load.call::<mlua::Value>("depth").is_ok());
    dir.write("depth.json", format!("{}1{}", "[".repeat(depth + 1), "]".repeat(depth + 1)));
    let error = load.call::<mlua::Value>("depth").unwrap_err().to_string();
    assert!(error.contains("depth") && error.contains("nesting limit"), "{error}");
    Ok(())
}

#[test]
fn roots_discovery_and_require_cache_are_separate() -> mlua::Result<()> {
    let dir = Fixture::new();
    let first = dir.0.join("first");
    let second = dir.0.join("second");
    std::fs::create_dir_all(first.join("sub")).unwrap();
    std::fs::create_dir(&second).unwrap();
    std::fs::write(first.join("value.json"), "1").unwrap();
    std::fs::write(second.join("value.json"), "2").unwrap();
    std::fs::write(first.join("module.lua"), "return data.load('value')").unwrap();
    std::fs::write(first.join("sub/hidden.lua"), "return 1").unwrap();
    let lua = Lua::new();
    let a = environment(&lua, &first, LuaConfig::default().data_file_limit_bytes)?;
    let b = environment(&lua, &second, LuaConfig::default().data_file_limit_bytes)?;
    assert_eq!(lua.load("return require('module')").set_environment(a.clone()).eval::<i64>()?, 1);
    std::fs::write(first.join("value.json"), "3").unwrap();
    assert_eq!(lua.load("return require('module')").set_environment(a.clone()).eval::<i64>()?, 1);
    assert_eq!(lua.load("return data.load('value')").set_environment(a).eval::<i64>()?, 3);
    assert_eq!(lua.load("return data.load('value')").set_environment(b).eval::<i64>()?, 2);
    let config = LuaConfig { scripts_dir: first.to_string_lossy().into_owned(), ..Default::default() };
    let scripts = discover(&config).unwrap();
    assert_eq!(scripts.iter().map(|(_, _, name)| name.as_str()).collect::<Vec<_>>(), ["module"]);
    Ok(())
}

#[cfg(unix)]
#[test]
fn data_and_require_reject_symlink_escape() -> mlua::Result<()> {
    use std::os::unix::fs::symlink;
    let dir = Fixture::new();
    let root = dir.0.join("root");
    std::fs::create_dir(&root).unwrap();
    dir.write("outside.json", "1");
    dir.write("outside.lua", "return 1");
    symlink(dir.0.join("outside.json"), root.join("escape.json")).unwrap();
    symlink(dir.0.join("outside.lua"), root.join("escape.lua")).unwrap();
    let lua = Lua::new();
    let env = environment(&lua, &root, LuaConfig::default().data_file_limit_bytes)?;
    let load: Function = env.get::<Table>("data")?.get("load")?;
    assert!(load.call::<mlua::Value>("escape").unwrap_err().to_string().contains("escape"));
    assert!(env.get::<Function>("require")?.call::<mlua::Value>("escape").is_err());
    Ok(())
}

#[test]
fn data_allocations_respect_lua_memory_limit() -> mlua::Result<()> {
    let dir = Fixture::new();
    dir.write("large.json", format!("\"{}\"", "x".repeat(1024 * 1024)));
    let lua = Lua::new();
    let env = environment(&lua, &dir.0, LuaConfig::default().data_file_limit_bytes)?;
    let load: Function = env.get::<Table>("data")?.get("load")?;
    lua.set_memory_limit(lua.used_memory() + 64 * 1024)?;
    let error = load.call::<mlua::Value>("large").unwrap_err().to_string();
    assert!(error.contains("large"), "{error}");
    Ok(())
}
