use super::*;

#[test]
fn boundary_rejects_invalid_ids_amounts_and_recipient_shapes() {
    assert!(positive(0).is_err());
    assert!(positive(-1).is_err());
    assert_eq!(positive(7).unwrap(), 7);
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY] { assert!(amount(value).is_err()); }
    assert_eq!(amount(1.5).unwrap(), crate::isk::IskAmount::from_cents(150).unwrap());
    let lua = Lua::new();
    for source in ["{[2]=1}", "{[1]=1, x=2}", "{0}"] {
        let table = lua.load(format!("return {source}")).eval::<Table>().unwrap();
        assert!(ids(table).is_err(), "{source}");
    }
    let table = lua.load("return {7,8}").eval::<Table>().unwrap();
    assert_eq!(ids(table).unwrap(), vec![7, 8]);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn registered_read_functions_query_real_dedicated_database() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let lua = Lua::new();
    let env = lua.create_table().unwrap();
    register(&lua, &env, &db, &sde).unwrap();
    let dao: Table = env.get("dao").unwrap();
    let items: Table = dao.get("items").unwrap();
    for name in ["by_owner", "by_location"] {
        let function: mlua::Function = items.get(name).unwrap();
        let rows: Table = function.call_async(i64::MAX).await.unwrap();
        assert_eq!(rows.raw_len(), 0);
    }
    let characters: Table = dao.get("characters").unwrap();
    for name in ["get", "name", "ship"] {
        let function: mlua::Function = characters.get(name).unwrap();
        assert!(matches!(function.call_async::<mlua::Value>(i64::MAX).await.unwrap(), mlua::Value::Nil));
    }
    let skills: Table = dao.get("skills").unwrap();
    let levels: mlua::Function = skills.get("levels").unwrap();
    assert_eq!(levels.call_async::<Table>(i64::MAX).await.unwrap().raw_len(), 0);
    let corps: Table = dao.get("corps").unwrap();
    let corp: mlua::Function = corps.get("of").unwrap();
    assert!(matches!(corp.call_async::<mlua::Value>(i64::MAX).await.unwrap(), mlua::Value::Nil));
    let standings: Table = dao.get("standings").unwrap();
    let get: mlua::Function = standings.get("get").unwrap();
    assert!(matches!(get.call_async::<mlua::Value>((i64::MAX, i64::MAX)).await.unwrap(), mlua::Value::Nil));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with SDE"]
async fn registered_market_and_hangar_observers_filter_and_validate() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let item_type = sde.types.iter().find(|t| t.type_id > 0 && t.group_id.is_some() && t.category_id.is_some()).unwrap();
    let (type_id, group, category) = (item_type.type_id, item_type.group_id.unwrap(), item_type.category_id.unwrap());
    let other_type = sde.types.iter().find(|t| t.type_id > 0 && t.type_id != type_id).unwrap().type_id;
    let station = crate::client::filetime_now();
    let owner = station + 2;
    let mut order_ids = Vec::new();
    for (at, ty, bid, volume, price, minimum, range) in [
        (station, type_id, true, 7_i64, 10.0, 2_i64, 3),
        (station, type_id, true, 8, 20.0, 4, 5),
        (station, type_id, false, 9, 30.0, 6, 7),
        (station, type_id, false, 10, 40.0, 8, 9),
        (station + 1, type_id, true, 1, 1.0, 1, 1),
        (station, other_type, false, 1, 1.0, 1, 1),
        (station + 1, type_id, false, 1, 1.0, 1, 1),
        (station, other_type, true, 1, 1.0, 1, 1),
        (station, type_id, true, 0, 1.0, 1, 1),
        (station, type_id, false, 0, 1.0, 1, 1),
    ] {
        let id: i64 = db.query_one("INSERT INTO game.market_orders (type_id, station_id, solar_system_id, constellation_id, region_id, price, vol_entered, vol_remaining, min_volume, bid, range, duration) VALUES ($1,$2,$2,$2,$2,$3::float8::numeric,10,$4,$5,$6,$7,1) RETURNING order_id", &[&ty, &at, &price, &volume, &minimum, &bid, &range]).await.unwrap().get("order_id");
        order_ids.push(id);
    }
    let mut item_ids = Vec::new();
    for (who, at, flag) in [
        (owner, station, infinity_core::consts::FLAG_HANGAR),
        (owner + 1, station, infinity_core::consts::FLAG_HANGAR),
        (owner, station + 1, infinity_core::consts::FLAG_HANGAR),
        (owner, station, infinity_core::consts::FLAG_CARGO),
        (owner, station, infinity_core::consts::FLAG_CORP_DELIVERIES),
    ] {
        let id: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1,$2,$3,$4,5,false) RETURNING item_id", &[&type_id, &who, &at, &flag]).await.unwrap().get("item_id");
        item_ids.push(id);
    }
    let test_db = db.clone();
    let test_orders = order_ids.clone();
    let test_items = item_ids.clone();
    let result = tokio::spawn(async move {
        let lua = Lua::new();
        let env = lua.create_table().unwrap();
        register(&lua, &env, &test_db, &sde).unwrap();
        let globals = lua.globals();
        globals.set("dao", env.get::<Table>("dao").unwrap()).unwrap();
        globals.set("station", station).unwrap();
        globals.set("owner", owner).unwrap();
        globals.set("type_id", type_id).unwrap();
        globals.set("group_id", group).unwrap();
        globals.set("category_id", category).unwrap();
        globals.set("order_ids", lua.create_sequence_from(test_orders).unwrap()).unwrap();
        globals.set("item_id", test_items[0]).unwrap();
        lua.load(r#"
            local function check_orders(name, start, price, volume, minimum, range)
                local rows = dao.market[name](station, type_id)
                assert(#rows == 2)
                for i, row in ipairs(rows) do
                    local count = 0
                    for k in pairs(row) do
                        assert(k == "order_id" or k == "price" or k == "volume_remaining" or k == "min_volume" or k == "range")
                        count = count + 1
                    end
                    assert(count == 5 and row.order_id == order_ids[start + i - 1])
                    assert(row.price == price + (i - 1) * 10)
                    assert(row.volume_remaining == volume + i - 1)
                    assert(row.min_volume == minimum + (i - 1) * 2)
                    assert(row.range == range + (i - 1) * 2)
                end
                for _, args in ipairs({{0, type_id}, {-1, type_id}, {station, 0}, {station, -1}, {station, 2147483648}}) do
                    assert(not pcall(dao.market[name], args[1], args[2]))
                end
            end
            check_orders("bids", 1, 10, 7, 2, 3)
            check_orders("asks", 3, 30, 9, 6, 7)
            local rows = dao.items.in_hangar(owner, station)
            assert(#rows == 1)
            local row = rows[1]
            assert(row.item_id == item_id and row.type_id == type_id and row.owner_id == owner)
            assert(row.location_id == station and row.quantity == 5 and row.singleton == false)
            assert(row.group_id == group_id and row.category_id == category_id)
            for _, args in ipairs({{0, station}, {-1, station}, {owner, 0}, {owner, -1}}) do
                assert(not pcall(dao.items.in_hangar, args[1], args[2]))
            end
        "#).exec_async().await.unwrap();
    }).await;
    db.execute("DELETE FROM game.market_orders WHERE order_id=ANY($1)", &[&order_ids]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&item_ids]).await.unwrap();
    result.unwrap();
}
