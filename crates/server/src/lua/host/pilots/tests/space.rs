//! Registered Lua spatial operations against a dedicated database and system actor.
use super::*;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn spatial_operations_authorize_and_use_actor() {
    let r = fixture::resources().await;
    let (race, bloodline, school, station) = fixture::creation_inputs(&r).await;
    let fields = fixture::FieldSnapshot::capture(&r, station).await;
    let fixture_system = r.sde.map.station(station).unwrap().solar_system_id.unwrap();
    assert!(r.sde.map.celestials(fixture_system).any(|celestial| celestial.group_id == Some(i64::from(crate::consts::GROUP_ASTEROID_BELT))), "fixture system must contain SDE asteroid belts");
    let lua = Lua::new();
    register(&lua, &lua.globals(), &r).unwrap();
    let globals = lua.globals();
    crate::lua::host::world::register(&lua, &globals, &r).unwrap();
    globals.set("race", race).unwrap();
    globals.set("bloodline", bloodline).unwrap();
    globals.set("school", school).unwrap();
    globals.set("station", station).unwrap();
    globals.set("pilot_name", format!("SpacePilot{}", crate::space::now_ms())).unwrap();
    let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("id", id).unwrap();
    globals.set("pilot_name", format!("SpacePeer{}", crate::space::now_ms())).unwrap();
    let peer: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("peer", peer).unwrap();
    let foreign: i64 = r.db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('space-foreign-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"test"[..]]).await.unwrap().get("user_id");
    let original_ship = Arc::new(Mutex::new(None));
    let test_ship = original_ship.clone();
    let test_r = r.clone();
    let result = tokio::spawn(async move {
        let r = test_r;
        let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        for call in ["pilots.undock(id)", "pilots.dock(id,station)", "pilots.stop(id)",
            "pilots.orbit(id,station,1)", "pilots.approach(id,station)",
            "pilots.warp_to(id,{x=0,y=0,z=0})", "pilots.warp_to_ball(id,station)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "offline: {call}");
        }
        assert!(lua.load("return pilots.condition(id)==nil and #pilots.targeters(id)==0").eval_async::<bool>().await?);
        lua.load("pilots.online(peer)").exec_async().await?;
        let peer_session = r.world.online_session(peer).unwrap();
        let (peer_tx, mut peer_rx) = tokio::sync::mpsc::unbounded_channel();
        r.world.register_pilot(peer, peer_tx);
        r.world.set_online_session(peer, Some(peer_session));
        lua.load("pilots.online(id)").exec_async().await?;
        let arrival = peer_rx.try_recv()?;
        assert_eq!(arrival.0, "OnCharNowInStation");
        assert_eq!(arrival.1, vec![crate::services::presence::guest_record(&r.world.online_session(id).unwrap())]);
        assert!(lua.load("local c=pilots.condition(id); return c == nil or c.capacitor == nil").eval_async::<bool>().await?);
        for call in ["pilots.orbit(id,station,-1)", "pilots.orbit(id,station,1e300)",
            "pilots.orbit(id,station,0/0)", "pilots.approach(id,0)",
            "pilots.warp_to(id,{x=0,y=0,z=1/0})", "pilots.warp_to(id,{x=0,y=0,z=0,w=0})"] {
            assert!(lua.load(call).exec_async().await.is_err(), "invalid: {call}");
        }
        assert!(lua.load("pilots.dock(id,station)").exec_async().await.is_err());
        assert_eq!(r.world.online_session(id).unwrap().station_id, Some(station));
        lua.load("pilots.undock(id)").exec_async().await?;
        let (notice, args) = peer_rx.try_recv()?;
        assert_eq!(notice, "OnCharNoLongerInStation");
        let mut departed = r.world.online_session(id).unwrap();
        departed.station_id = Some(station);
        assert_eq!(args, vec![crate::services::presence::guest_record(&departed)]);
        let identity: Table = lua.load("return pilots.identity(id)").eval_async().await?;
        let ship: i64 = identity.get("ship_id")?;
        *test_ship.lock().unwrap() = Some(ship);
        let system: i64 = identity.get("system_id")?;
        assert_eq!(system, fixture_system);
        globals.set("system", system)?;
        let rock_ids: Table = lua.load("local rocks = world.balls(system, {category_id=25}); assert(#rocks > 0, 'undock must expose asteroid balls'); local ids = {}; for i, rock in ipairs(rocks) do ids[i] = rock.id end; return ids").eval_async().await?;
        let rock_ids: Vec<i64> = rock_ids.sequence_values::<i64>().collect::<mlua::Result<Vec<_>>>()?;
        for rock in rock_ids {
            let row = r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2", &[&rock, &system]).await?;
            assert!(row.get::<_, i64>("quantity") > 0);
        }
        assert_eq!(r.world.online_session(id).unwrap().station_id, None);
        assert_eq!(r.world.with(system, move |s| s.pilot_ball(id).map(|b| b.id)).await.flatten(), Some(ship));
        let location = r.db.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&id]).await?;
        assert_eq!(location.get::<_, Option<i64>>("station_id"), None);
        assert_eq!(location.get::<_, i64>("solar_system_id"), system);
        let condition: Table = lua.load("return pilots.condition(id)").eval_async().await?;
        assert!(condition.get::<Option<f64>>("capacitor")?.is_some());
        for field in ["shield", "armor_damage", "hull_damage"] { assert!(condition.get::<Option<f64>>(field)?.is_some(), "{field}"); }
        let snapshot = crate::services::combat::groups_health::capacitor_snapshot(&r.db, &r.world, system, ship).await.ok().unwrap().unwrap();
        let lua_cap = condition.get::<f64>("capacitor")?;
        assert!(lua_cap.is_finite() && lua_cap >= 0.0 && lua_cap <= snapshot.capacity);
        assert_eq!(lua.load("return #pilots.targeters(id)").eval_async::<i64>().await?, 0);
        lua.load("pilots.undock(peer)").exec_async().await?;
        let peer_identity: Table = lua.load("return pilots.identity(peer)").eval_async().await?;
        let peer_ship: i64 = peer_identity.get("ship_id")?;
        let before = r.world.ball(system, peer_ship).await.unwrap();
        r.db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&id, &peer_ship]).await?;
        for call in ["pilots.stop(id)", "pilots.orbit(id,station,1000)", "pilots.approach(id,station)", "pilots.warp_to(id,{x=0,y=0,z=0})", "pilots.warp_to_ball(id,station)"] {
            lua.load(call).exec_async().await?;
        }
        assert!(r.world.after(system, 1100).await);
        let after = r.world.ball(system, peer_ship).await.unwrap();
        assert_eq!((after.mode, after.goto, after.follow_id), (before.mode, before.goto, before.follow_id));
        assert!(lua.load("pilots.dock(id,station)").exec_async().await.is_err());
        r.db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&id, &ship]).await?;
        let peer_station_pos = r.world.with(system, move |s| s.ball(station).map(|b| b.pos)).await.flatten().unwrap();
        r.world.with(system, move |s| s.set_ball_position(peer_ship, peer_station_pos, |_, _| true)).await.unwrap();
        assert!(r.world.after(system, 1100).await);
        lua.load("pilots.dock(peer,station)").exec_async().await?;
        let original = r.world.remove_ball(system, ship).await.unwrap();
        let mut wrong = original.clone();
        wrong.char_id = Some(peer);
        r.world.add_ship(system, wrong).await?;
        for call in ["pilots.stop(id)", "pilots.orbit(id,station,1000)", "pilots.approach(id,station)", "pilots.warp_to(id,{x=0,y=0,z=0})", "pilots.warp_to_ball(id,station)"] {
            lua.load(call).exec_async().await?;
        }
        assert!(r.world.after(system, 1100).await);
        let wrong = r.world.ball(system, ship).await.unwrap();
        assert_eq!((wrong.char_id, wrong.mode, wrong.goto, wrong.follow_id), (Some(peer), original.mode, original.goto, original.follow_id));
        assert_eq!(lua.load("return #pilots.targeters(id)").eval_async::<i64>().await?, 0);
        let missing_actor: Table = lua.load("return pilots.condition(id)").eval_async().await?;
        for field in ["shield", "armor_damage", "hull_damage"] { assert!(missing_actor.get::<Option<f64>>(field)?.is_some(), "{field}"); }
        assert!(missing_actor.get::<Option<f64>>("capacitor")?.is_none());
        assert!(lua.load("pilots.dock(id,station)").exec_async().await.is_err());
        r.world.remove_ball(system, ship).await;
        r.world.add_ship(system, original).await?;
        let npc = -id;
        let mut npc_ball = r.world.ball(system, ship).await.unwrap();
        npc_ball.id = npc;
        npc_ball.char_id = None;
        r.world.add_ship(system, npc_ball).await?;
        r.world.with(system, move |s| {
            let mut brain = crate::space::NpcBrain::new(None, crate::space::NpcConfig {
                fly_range: 1.0, aggro_range: 1e9, rate_of_fire_ms: 1e9,
                scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0),
            }, Arc::new(|_| Box::pin(async {})));
            brain.target = Some((ship, id, 0));
            brain.announced = true;
            assert!(s.add_npc(npc, brain));
        }).await.unwrap();
        let targeters: Table = lua.load("return pilots.targeters(id)").eval_async().await?;
        assert_eq!(targeters.raw_len(), 1);
        assert_eq!(targeters.raw_get::<i64>(1)?, npc);
        r.world.remove_ball(system, npc).await;
        lua.load("pilots.orbit(id,station,1000)").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        assert_eq!(r.world.ball(system, ship).await.unwrap().follow_id, station);
        lua.load("pilots.approach(id,station)").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        assert_eq!(r.world.ball(system, ship).await.unwrap().follow_id, station);
        lua.load("pilots.stop(id)").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        assert_eq!(r.world.ball(system, ship).await.unwrap().mode, crate::space::MODE_STOP);
        let position = r.world.with(system, move |s| s.sample_position(ship)).await.flatten().unwrap();
        globals.set("x", position[0] + 1000.0)?;
        globals.set("y", position[1])?;
        globals.set("z", position[2])?;
        lua.load("pilots.warp_to(id,{x=x,y=y,z=z})").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        assert_eq!(r.world.ball(system, ship).await.unwrap().goto, [position[0] + 1000.0, position[1], position[2]]);
        lua.load("pilots.stop(id)").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        let stopped = r.world.ball(system, ship).await.unwrap();
        lua.load("pilots.warp_to_ball(id,9223372036854775807)").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        let unchanged = r.world.ball(system, ship).await.unwrap();
        assert_eq!((unchanged.mode, unchanged.goto), (stopped.mode, stopped.goto));
        let station_pos = r.world.with(system, move |s| s.ball(station).map(|b| b.pos)).await.flatten().unwrap();
        r.world.with(system, move |s| s.set_ball_position(ship, [station_pos[0] + 1_000_000.0, station_pos[1], station_pos[2]], |_, _| true)).await.unwrap();
        assert!(r.world.after(system, 1100).await);
        assert!(lua.load("pilots.dock(id,station)").exec_async().await.is_err());
        assert_eq!(r.world.online_session(id).unwrap().station_id, None);
        assert!(peer_rx.try_recv().is_err());
        assert!(r.world.ball(system, ship).await.is_some());
        let location = r.db.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&id]).await?;
        assert_eq!((location.get::<_, Option<i64>>("station_id"), location.get::<_, i64>("solar_system_id")), (None, system));
        r.world.with(system, move |s| s.set_ball_position(ship, station_pos, |_, _| true)).await.unwrap();
        assert!(r.world.after(system, 1100).await);
        lua.load("pilots.dock(id,station)").exec_async().await?;
        let current = r.world.online_session(id).unwrap();
        assert_eq!((current.station_id, current.system_id), (Some(station), Some(system)));
        assert!(r.world.ball(system, ship).await.is_none());
        let location = r.db.query_one("SELECT station_id, solar_system_id FROM game.characters WHERE character_id=$1", &[&id]).await?;
        assert_eq!((location.get::<_, Option<i64>>("station_id"), location.get::<_, i64>("solar_system_id")), (Some(station), system));
        let item = r.db.query_one("SELECT location_id, flag_id FROM game.items WHERE item_id=$1", &[&ship]).await?;
        assert_eq!((item.get::<_, i64>("location_id"), item.get::<_, i32>("flag_id")), (station, crate::consts::FLAG_HANGAR));
        let (notice, args) = peer_rx.try_recv()?;
        assert_eq!(notice, "OnCharNowInStation");
        assert_eq!(args, vec![crate::services::presence::guest_record(&current)]);
        let docked: Table = lua.load("return pilots.condition(id)").eval_async().await?;
        for field in ["shield", "armor_damage", "hull_damage"] { assert!(docked.get::<Option<f64>>(field)?.is_some(), "{field}"); }
        assert!(docked.get::<Option<f64>>("capacitor")?.is_none());
        r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&id, &foreign]).await?;
        for call in ["pilots.undock(id)", "pilots.dock(id,station)", "pilots.stop(id)", "pilots.orbit(id,station,1)", "pilots.approach(id,station)", "pilots.warp_to(id,{x=0,y=0,z=0})", "pilots.warp_to_ball(id,station)", "pilots.condition(id)", "pilots.targeters(id)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "foreign: {call}");
        }
        r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&id, &r.pilots.account]).await?;
        lua.load("pilots.offline(id)").exec_async().await?;
        r.db.execute("UPDATE game.characters SET deleted=true WHERE character_id=$1", &[&id]).await?;
        for call in ["pilots.stop(id)", "pilots.condition(id)", "pilots.targeters(id)"] {
            assert!(lua.load(call).exec_async().await.is_err());
        }
        r.db.execute("UPDATE game.characters SET deleted=false WHERE character_id=$1", &[&id]).await?;
        lua.load("pilots.online(id); pilots.undock(id)").exec_async().await?;
        r.world.with(system, move |s| s.set_ball_position(ship, [station_pos[0] + 1_000_000.0, station_pos[1], station_pos[2]], |_, _| true)).await.unwrap();
        assert!(r.world.after(system, 1100).await);
        let destination = r.world.with(system, move |s| {
            s.warp_in(station).unwrap_or_else(|| s.ball(station).unwrap().position_at(s.now_ms()))
        }).await.unwrap();
        lua.load("pilots.warp_to_ball(id,station)").exec_async().await?;
        assert!(r.world.after(system, 1100).await);
        let after = r.world.ball(system, ship).await.unwrap();
        assert_eq!(after.mode, crate::space::MODE_WARP);
        assert_eq!(after.goto, destination);
        Ok(())
        }.await;
        result.unwrap();
    }).await;
    let saved_ship = *original_ship.lock().unwrap();
    if let Some(ship) = saved_ship {
        r.db.execute("UPDATE game.characters SET ship_id=$2 WHERE character_id=$1", &[&id, &ship]).await.unwrap();
    }
    r.db.execute("UPDATE game.characters SET user_id=$2, deleted=false WHERE character_id=$1", &[&id, &r.pilots.account]).await.unwrap();
    r.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&foreign]).await.unwrap();
    let _ = lifecycle::offline(r.clone(), peer).await;
    fixture::cleanup(&r.db, peer).await;
    let _ = lifecycle::offline(r.clone(), id).await;
    fixture::cleanup(&r.db, id).await;
    fields.restore(&r).await;
    result.unwrap();
}
