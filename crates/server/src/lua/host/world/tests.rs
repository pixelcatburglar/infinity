use super::*;

#[test]
fn operation_shapes_are_strict_and_wire_arguments_are_typed() {
    let lua = Lua::new();
    crate::lua::value::register(&lua, &lua.globals()).unwrap();
    let table = |source: &str| lua.load(source).eval::<Table>().unwrap();
    assert!(operation("notify", table("{character=1,event='OnTest',args={eve.int(2)}}")).is_ok());
    for source in ["{character=1,event='x',args={[1]=eve.int(1),[3]=eve.int(3)}}", "{character=1,event='x',args={{}}}", "{character=1,event='x',args={},extra=true}"] {
        assert!(operation("notify", table(source)).is_err());
    }
    assert!(operation("remove_ball", table("{id=1}")).is_ok());
    assert!(operation("remove_ball", table("{id=0}")).is_err());
    assert!(operation("remove_ball", table("{id=1,extra=1}")).is_err());
    assert!(operation("spawn_npc", table("{type_id=1,corp=1,position={1,2,3},aggro_target=1}")).is_ok());
    for source in ["{type_id=1,corp=1,position={1,2}}", "{type_id=1,corp=1,position={1,2,math.huge}}", "{type_id=1,corp=1,position={1,2,3},aggro_target=0}", "{type_id=1,corp=1,position={1,2,3},extra=1}"] {
        assert!(operation("spawn_npc", table(source)).is_err());
    }
    assert!(operation("other", table("{}")).is_err());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with initialized SDE"]
async fn world_host_routes_notifications_and_refuses_missing_system() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = Resources { db, sde: sde.clone(), world: world.clone(), pilots, store_value_limit_bytes: 1024 };
    let lua = Lua::new();
    crate::lua::value::register(&lua, &lua.globals()).unwrap();
    register(&lua, &lua.globals(), &resources).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(1, tx);
    lua.load("world.notify_pilot(1, 'OnTest', {eve.int(42)})").exec().unwrap();
    let (name, args) = rx.try_recv().unwrap();
    assert_eq!(name, "OnTest");
    assert!(matches!(args.as_slice(), [marshal::Value::Int(42)]));
    assert!(lua.load("world.notify_pilot(1, 'OnTest', {[2]=eve.int(42)})").exec().is_err());
    assert!(lua.load("return world.ball(1, 1)").eval_async::<Option<Table>>().await.unwrap().is_none());
    assert!(!lua.load("return world.schedule(1, 0, 'remove_ball', {id=1})").eval_async::<bool>().await.unwrap());
    assert!(!lua.load("return world.schedule(1, 0, 'notify', {character=1,event='OnTest',args={}})").eval_async::<bool>().await.unwrap());
    assert!(lua.load("return world.schedule(1, -1, 'remove_ball', {id=1})").eval_async::<bool>().await.is_err());
    assert!(lua.load("return world.schedule(1, 0, 'spawn_npc', {type_id=2147483647,corp=1,position={1,2,3}})").eval_async::<bool>().await.is_err());
    world.system(1).await.unwrap();
    assert!(lua.load("return world.schedule(1, 0, 'notify', {character=1,event='OnScheduled',args={eve.int(7)}})").eval_async::<bool>().await.unwrap());
    assert!(world.after(1, 0).await);
    let (name, args) = rx.try_recv().unwrap();
    assert_eq!(name, "OnScheduled");
    assert!(matches!(args.as_slice(), [marshal::Value::Int(7)]));
    let type_id = sde.types.iter().find(|data| data.group_id
        .and_then(|id| sde.types.group(id))
        .is_some_and(|group| group.category_id == Some(CATEGORY_ENTITY)))
        .expect("dedicated SDE must contain an entity type").type_id;
    let (destiny_tx, mut destiny_rx) = tokio::sync::mpsc::unbounded_channel();
    world.observe(1, 1, 1, destiny_tx).await.unwrap();
    lua.globals().set("npc_type", type_id).unwrap();
    assert!(lua.load("return world.schedule(1, 0, 'spawn_npc', {type_id=npc_type,corp=1,position={100,200,300}})").eval_async::<bool>().await.unwrap());
    tokio::time::timeout(std::time::Duration::from_secs(10), destiny_rx.recv()).await.expect("spawn did not broadcast").expect("observer closed");
    let id = world.with(1, move |s| s.balls().find(|b| b.type_id == type_id && b.owner_id == 1).map(|b| b.id)).await.unwrap().expect("spawned NPC not present");
    lua.globals().set("npc_id", id).unwrap();
    let ball = lua.load("return world.ball(1, npc_id)").eval_async::<Table>().await;
    let removed = lua.load("return world.schedule(1, 0, 'remove_ball', {id=npc_id})").eval_async::<bool>().await;
    let processed = world.after(1, 0).await;
    assert!(removed.unwrap());
    assert!(processed);
    let ball = ball.unwrap();
    assert_eq!(ball.get::<i64>("id").unwrap(), id);
    assert_eq!(ball.get::<i32>("type_id").unwrap(), type_id);
    let position: Table = ball.get("position").unwrap();
    for (index, expected) in [100.0, 200.0, 300.0].into_iter().enumerate() {
        let coordinate = position.get::<f64>(index + 1).unwrap();
        assert!(coordinate.is_finite());
        assert_eq!(coordinate, expected);
    }
    assert!(lua.load("return world.ball(1, npc_id)").eval_async::<Option<Table>>().await.unwrap().is_none());
}

fn ball(id: i64, type_id: i32) -> crate::space::Ball {
    crate::space::Ball {
        id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with initialized SDE"]
async fn scheduled_removal_refuses_persistent_ships_at_schedule_and_execution_but_allows_drones() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::deployment::DatabaseConfig { schema_on_start: false, ..crate::config::tests::test_database() }).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).expect("SDE ship").type_id;
    let drone_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_DRONE)).expect("SDE drone").type_id;
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = Resources { db, sde, world: world.clone(), pilots, store_value_limit_bytes: 1024 };
    let lua = Lua::new();
    register(&lua, &lua.globals(), &resources).unwrap();
    world.system(1).await.unwrap();
    let (existing, later, drone) = (i64::MAX - 2, i64::MAX - 1, i64::MAX);
    world.with(1, move |s| {
        s.add_persistent_ship(ball(existing, ship_type));
        s.add_persistent_ship(ball(drone, drone_type));
    }).await.unwrap();
    lua.globals().set("existing", existing).unwrap();
    lua.globals().set("drone", drone).unwrap();
    let error = lua.load("return world.schedule(1, 0, 'remove_ball', {id=existing})")
        .eval_async::<bool>().await.unwrap_err();
    assert!(error.to_string().contains("persistent ship hull"));
    world.with(1, move |s| s.add_persistent_ship(ball(later, ship_type))).await.unwrap();
    let sde = Arc::clone(&resources.sde);
    world.schedule(1, 0, move |s| remove_scheduled_ball(s, &sde, 1, later));
    assert!(lua.load("return world.schedule(1, 0, 'remove_ball', {id=drone})").eval_async::<bool>().await.unwrap());
    assert!(world.after(1, 0).await);
    assert!(world.ball(1, existing).await.is_some());
    assert!(world.ball(1, later).await.is_some());
    assert!(world.ball(1, drone).await.is_none());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated test database with initialized SDE"]
async fn world_observations_filter_sampled_balls_and_read_static_celestials_without_loading() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let first = sde.types.iter().find(|t| t.group_id.is_some_and(|id| id > 0) && t.category_id.is_some_and(|id| id > 0)).expect("SDE type with group and category");
    let second = sde.types.iter().find(|t| t.group_id.is_some_and(|id| id > 0 && Some(id) != first.group_id)
        && t.category_id.is_some_and(|id| id > 0 && Some(id) != first.category_id)).expect("SDE type in another group and category");
    let (first_type, first_group, first_category) = (first.type_id, first.group_id.unwrap(), first.category_id.unwrap());
    let (second_type, second_group, second_category) = (second.type_id, second.group_id.unwrap(), second.category_id.unwrap());
    let static_system = sde.map.all_celestials().find(|c| c.group_id == Some(i64::from(GROUP_ASTEROID_BELT)) && c.solar_system_id.is_some_and(|id| id > 0))
        .expect("SDE asteroid belt").solar_system_id.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let hub = crate::chat::Hub::new("test", db.clone(), world.clone());
    let pilots = crate::lua::host::pilots::Pilots::new(&db, hub).await.unwrap();
    let resources = Resources { db, sde: sde.clone(), world: world.clone(), pilots, store_value_limit_bytes: 1024 };
    let lua = Lua::new();
    register(&lua, &lua.globals(), &resources).unwrap();
    lua.globals().set("static_system", static_system).unwrap();
    assert!(world.with(static_system, |_| ()).await.is_none());
    for (name, belts) in [("celestials", false), ("asteroid_belts", true)] {
        let rows: Table = lua.load(format!("return world.{name}(static_system)")).eval().unwrap();
        let expected: Vec<_> = sde.map.celestials(static_system).filter(|c| !belts || c.group_id == Some(i64::from(GROUP_ASTEROID_BELT))).collect();
        assert_eq!(rows.raw_len(), expected.len());
        for (index, celestial) in expected.iter().enumerate() {
            let row: Table = rows.raw_get(index + 1).unwrap();
            assert_eq!(row.raw_get::<i64>("id").unwrap(), celestial.item_id);
            assert_eq!(row.raw_get::<Option<i64>>("type_id").unwrap(), celestial.type_id);
            assert_eq!(row.raw_get::<Option<i64>>("group_id").unwrap(), celestial.group_id);
            let position: Table = row.raw_get("position").unwrap();
            for (i, value) in [celestial.x, celestial.y, celestial.z].into_iter().enumerate() {
                assert_eq!(position.raw_get::<Option<f64>>(i + 1).unwrap(), value);
            }
        }
        assert!(lua.load(format!("return world.{name}(0)")).eval::<Table>().is_err());
        assert_eq!(lua.load(format!("return world.{name}({})", i64::MAX)).eval::<Table>().unwrap().raw_len(), 0);
    }
    assert!(world.with(static_system, |_| ()).await.is_none());
    assert_eq!(lua.load("return world.balls(1, nil)").eval_async::<Table>().await.unwrap().raw_len(), 0);
    assert!(world.with(1, |_| ()).await.is_none());
    world.system(1).await.unwrap();
    let (first_id, second_id) = (i64::MAX - 1, i64::MAX);
    world.with(1, move |s| {
        let mut first = ball(first_id, first_type);
        first.pos = [100.0, 200.0, 300.0];
        first.old_pos = first.pos;
        let mut second = ball(second_id, second_type);
        second.pos = [400.0, 500.0, 600.0];
        second.old_pos = second.pos;
        s.add_ship(first);
        s.add_ship(second);
        s.set_ball_position(first_id, [110.0, 210.0, 310.0], |_, _| true);
    }).await.unwrap();
    assert!(world.after(1, 1200).await);
    for (name, value) in [("first_id", first_id), ("second_id", second_id), ("first_group", i64::from(first_group)),
        ("second_group", i64::from(second_group)), ("first_category", i64::from(first_category)), ("second_category", i64::from(second_category))] {
        lua.globals().set(name, value).unwrap();
    }
    lua.load(r#"
        local all = world.balls(1, nil)
        local seen = {}
        for _, row in ipairs(all) do
            if row.id == first_id or row.id == second_id then
                local single = world.ball(1, row.id)
                for _, key in ipairs({"id", "type_id", "owner_id", "radius"}) do assert(row[key] == single[key]) end
                for _, key in ipairs({"position", "velocity"}) do
                    assert(#row[key] == 3)
                    for i = 1, 3 do assert(row[key][i] == single[key][i]) end
                end
                if row.id == first_id then
                    assert(row.position[1] == 110 and row.position[2] == 210 and row.position[3] == 310)
                end
                seen[row.id] = true
            end
        end
        assert(seen[first_id] and seen[second_id])
        local function contains(rows, id)
            for _, row in ipairs(rows) do if row.id == id then return true end end
            return false
        end
        local category = world.balls(1, {category_id=first_category})
        assert(contains(category, first_id) and not contains(category, second_id))
        local group = world.balls(1, {group_id=second_group})
        assert(contains(group, second_id) and not contains(group, first_id))
        local both = world.balls(1, {category_id=first_category, group_id=first_group})
        assert(contains(both, first_id) and not contains(both, second_id))
        assert(#world.balls(1, {category_id=first_category, group_id=second_group}) == 0)
        assert(#world.balls(1, {category_id=second_category, group_id=first_group}) == 0)
    "#).exec_async().await.unwrap();
    for expression in ["world.balls(0)", "world.balls(1, {category_id=0})", "world.balls(1, {group_id=-1})",
        "world.balls(1, {category_id=2147483648})", "world.balls(1, {group_id=2147483648})", "world.balls(1, {extra=1})"] {
        assert!(lua.load(format!("return {expression}")).eval_async::<Table>().await.is_err(), "{expression}");
    }
}
