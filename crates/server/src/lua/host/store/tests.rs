use super::*;

#[test]
fn nested_values_round_trip_and_invalid_values_fail() {
    let lua = Lua::new();
    for source in ["true", "42", "1.5", "'text'", "{name='x', nested={1,2}}"] {
        let value: Value = lua.load(format!("return {source}")).eval().unwrap();
        let json = encode(value, "key", &mut HashSet::new(), &mut 65536, 0).unwrap();
        let restored = decode(&lua, json.clone(), "key", 0).unwrap();
        assert_eq!(encode(restored, "key", &mut HashSet::new(), &mut 65536, 0).unwrap(), json);
    }
    for source in ["nil", "function() end", "0/0", "1/0", "{[1]=1, x=2}", "{[2]=1}", "{[false]=1}", "(function() local t={} t.x=t return t end)()"] {
        let value: Value = lua.load(format!("return {source}")).eval().unwrap();
        let error = encode(value, "key", &mut HashSet::new(), &mut 65536, 0).unwrap_err().to_string();
        assert!(error.contains("key"), "{source}: {error}");
    }
    let deep: Value = lua.load("local t={} for i=1,70 do t={t} end return t").eval().unwrap();
    assert!(encode(deep, "deep", &mut HashSet::new(), &mut 65536, 0).is_err());
    let value: Value = lua.load("return '12345'").eval().unwrap();
    assert!(encode(value, "large", &mut HashSet::new(), &mut 6, 0).is_err());
}

#[test]
fn persisted_values_enforce_depth_and_name_the_key() {
    let lua = Lua::new();
    let mut value = serde_json::json!(true);
    for _ in 0..=MAX_DEPTH { value = serde_json::Value::Array(vec![value]); }
    assert!(decode(&lua, value, "deep-key", 0).unwrap_err().to_string().contains("deep-key"));
    assert!(decode(&lua, serde_json::Value::Null, "null-key", 0).unwrap_err().to_string().contains("null-key"));
}

#[test]
fn bulk_keys_require_dense_string_sequences() {
    let lua = Lua::new();
    assert!(sequence_keys(lua.create_table().unwrap()).unwrap().is_empty());
    for source in ["{[2]='x'}", "{'x', [3]='y'}", "{'x', extra='y'}", "{'x', 2}", "{[0]='x'}", "{[false]='x'}"] {
        let table: Table = lua.load(format!("return {source}")).eval().unwrap();
        assert!(sequence_keys(table).is_err(), "{source}");
    }
    let table: Table = lua.load("return {'x', 'x', 'y'}").eval().unwrap();
    assert_eq!(sequence_keys(table).unwrap(), vec!["x", "x", "y"]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database"]
async fn registered_stores_are_bound_to_their_own_script() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = super::super::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = super::super::Resources { db: db.clone(), sde, world, pilots, store_value_limit_bytes: 65536 };
    let lua = Lua::new();
    let a = lua.create_table().unwrap();
    let b = lua.create_table().unwrap();
    let suffix = format!("{}_{}", std::process::id(), crate::space::now_ms());
    let script_a = format!("fixture_a_{suffix}");
    let script_b = format!("fixture_b_{suffix}");
    let key = "isolation";
    assert!(lua_store::get(&db, &script_a, key).await.unwrap().is_none());
    assert!(lua_store::get(&db, &script_b, key).await.unwrap().is_none());
    super::super::register(&lua, &a, &script_a, &toml::Table::new(), &resources).unwrap();
    super::super::register(&lua, &b, &script_b, &toml::Table::new(), &resources).unwrap();
    assert!(a.get::<Option<Table>>("db").unwrap().is_none());
    assert!(b.get::<Option<Table>>("db").unwrap().is_none());
    let a: Table = a.get("store").unwrap();
    let b: Table = b.get("store").unwrap();
    let set: mlua::Function = a.get("set").unwrap();
    set.call_async::<()>((key, "alpha")).await.unwrap();
    let get_a: mlua::Function = a.get("get").unwrap();
    let get_b: mlua::Function = b.get("get").unwrap();
    assert_eq!(get_a.call_async::<String>(key).await.unwrap(), "alpha");
    assert!(matches!(get_b.call_async::<Value>(key).await.unwrap(), Value::Nil));
    let set_b: mlua::Function = b.get("set").unwrap();
    set_b.call_async::<()>((key, "beta")).await.unwrap();
    assert_eq!(get_a.call_async::<String>(key).await.unwrap(), "alpha");
    assert_eq!(get_b.call_async::<String>(key).await.unwrap(), "beta");
    let get_many: mlua::Function = a.get("get_many").unwrap();
    let get_many_b: mlua::Function = b.get("get_many").unwrap();
    let requested = lua.create_sequence_from([key, "missing", key]).unwrap();
    let result: Table = get_many.call_async(requested.clone()).await.unwrap();
    assert_eq!(result.get::<String>(key).unwrap(), "alpha");
    assert!(matches!(result.get::<Value>("missing").unwrap(), Value::Nil));
    let result_b: Table = get_many_b.call_async(requested).await.unwrap();
    assert_eq!(result_b.get::<String>(key).unwrap(), "beta");
    let empty: Table = get_many.call_async(lua.create_table().unwrap()).await.unwrap();
    assert_eq!(empty.pairs::<Value, Value>().count(), 0);
    let invalid: Table = lua.load("return {[2]='x'}").eval().unwrap();
    assert!(get_many.call_async::<Table>(invalid).await.is_err());
    let mut requested = Vec::new();
    for i in 0..100 {
        let key = format!("bulk_{i}");
        let value: Value = lua.load(format!("return {{number={i}, flag=true, text='typed'}}")).eval().unwrap();
        set.call_async::<()>((key.clone(), value)).await.unwrap();
        requested.push(key);
    }
    let result: Table = get_many.call_async(lua.create_sequence_from(requested.clone()).unwrap()).await.unwrap();
    for (i, key) in requested.iter().enumerate() {
        let value: Table = result.get(key.as_str()).unwrap();
        assert_eq!(value.get::<i64>("number").unwrap(), i as i64);
        assert!(value.get::<bool>("flag").unwrap());
        assert_eq!(value.get::<String>("text").unwrap(), "typed");
        assert!(lua_store::delete(&db, &script_a, key).await.unwrap());
    }
    let result_b: Table = get_many_b.call_async(lua.create_sequence_from(requested).unwrap()).await.unwrap();
    assert_eq!(result_b.pairs::<Value, Value>().count(), 0);
    let keys: mlua::Function = a.get("keys").unwrap();
    let listed: Table = keys.call_async("iso").await.unwrap();
    assert_eq!(listed.get::<String>(1).unwrap(), key);
    let delete_a: mlua::Function = a.get("delete").unwrap();
    let delete_b: mlua::Function = b.get("delete").unwrap();
    assert!(delete_a.call_async::<bool>(key).await.unwrap());
    assert!(delete_b.call_async::<bool>(key).await.unwrap());
    assert!(!delete_a.call_async::<bool>(key).await.unwrap());
    let oversized = serde_json::to_string(&"x".repeat(65537)).unwrap();
    lua_store::set(&db, &script_a, "oversized", &oversized).await.unwrap();
    assert!(get_a.call_async::<Value>("oversized").await.unwrap_err().to_string().contains("oversized"));
    let oversized_keys = lua.create_sequence_from(["oversized"]).unwrap();
    assert!(get_many.call_async::<Table>(oversized_keys).await.unwrap_err().to_string().contains("oversized"));
    assert!(delete_a.call_async::<bool>("oversized").await.unwrap());
    assert!(lua_store::keys(&db, &script_a, "").await.unwrap().is_empty());
    assert!(lua_store::keys(&db, &script_b, "").await.unwrap().is_empty());
}
