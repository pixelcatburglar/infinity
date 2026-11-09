//! Registered Lua effect and targeting operations against a dedicated database.
use super::*;

async fn deliveries(db: &crate::db::Db, id: i64) -> i64 {
    db.query_one("SELECT count(*) AS count FROM game.character_mining_ledger WHERE character_id=$1", &[&id]).await.unwrap().get("count")
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn mining_default_effect_and_target_lifecycle() {
    use infinity_core::{consts::dogma::{ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_SIGNATURE_RADIUS, EFFECT_MINING_LASER}};
    use crate::{dao::items::loading::ItemQuery, services::{combat, fit::{Fit, Key}, mining}};
    let r = fixture::resources().await;
    let (race, bloodline, school, station) = fixture::creation_inputs(&r).await;
    let fields = fixture::FieldSnapshot::capture(&r, station).await;
    let lua = Lua::new();
    register(&lua, &lua.globals(), &r).unwrap();
    let globals = lua.globals();
    globals.set("race", race).unwrap(); globals.set("bloodline", bloodline).unwrap();
    globals.set("school", school).unwrap(); globals.set("station", station).unwrap();
    globals.set("pilot_name", format!("MinerPilot{}", crate::space::now_ms())).unwrap();
    let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("id", id).unwrap();
    globals.set("pilot_name", format!("MinerPeer{}", crate::space::now_ms())).unwrap();
    let peer: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
    globals.set("peer", peer).unwrap();
    let foreign: i64 = r.db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('effects-foreign-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"test"[..]]).await.unwrap().get("user_id");
    let field = Arc::new(Mutex::new(None));
    let test_field = field.clone();
    let test_r = r.clone();
    let result = tokio::spawn(async move {
        let r = test_r;
        for call in ["pilots.activate(id,1,nil,nil)", "pilots.deactivate(id,1)", "pilots.lock_target(id,1)", "pilots.unlock_target(id,1)", "pilots.activate(id,0,nil,nil)", "pilots.deactivate(id,0)", "pilots.lock_target(id,0)", "pilots.unlock_target(id,0)", "pilots.activate(0,1,nil,nil)", "pilots.deactivate(0,1)", "pilots.lock_target(0,1)", "pilots.unlock_target(0,1)", "pilots.activate(id,1,0,nil)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "{call}");
        }
        lua.load("pilots.online(id)").exec_async().await.unwrap();
        for call in ["pilots.activate(id,1,nil,nil)", "pilots.deactivate(id,1)", "pilots.lock_target(id,1)", "pilots.unlock_target(id,1)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "docked: {call}");
        }
        lua.load("pilots.undock(id)").exec_async().await.unwrap();
        let identity: Table = lua.load("return pilots.identity(id)").eval_async().await.unwrap();
        let ship: i64 = identity.get("ship_id").unwrap();
        let system: i64 = identity.get("system_id").unwrap();
        let items = crate::services::items::load_items(&r.db, &r.sde, ItemQuery::Location(ship)).await.unwrap();
        let module = items.iter().find(|item| r.sde.types.effects(item.type_id).iter().any(|effect| effect.is_default && effect.effect_id == EFFECT_MINING_LASER)).expect("rookie mining laser with SDE default");
        let effect = r.sde.dogma.effect(EFFECT_MINING_LASER).unwrap();
        assert!(effect.name.is_some());
        let module_id = module.item_id;
        globals.set("module", module_id).unwrap();
        let no_default_type = r.sde.types.iter().find(|ty| !r.sde.types.effects(ty.type_id).iter().any(|effect| effect.is_default)).expect("SDE type without a default effect").type_id;
        let no_default: i64 = r.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&no_default_type, &id, &ship, &infinity_core::consts::FLAG_CARGO]).await.unwrap().get("item_id");
        globals.set("no_default", no_default).unwrap();
        for call in ["pilots.activate(id,no_default,nil,nil)", "pilots.deactivate(id,no_default)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "{call}");
        }
        lua.load("pilots.online(peer); pilots.offline(peer)").exec_async().await.unwrap();
        let peer_ship: i64 = r.db.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&peer]).await.unwrap().get("ship_id");
        let peer_items = crate::services::items::load_items(&r.db, &r.sde, ItemQuery::Location(peer_ship)).await.unwrap();
        let foreign_module = peer_items.iter().find(|item| r.sde.types.effects(item.type_id).iter().any(|effect| effect.is_default && effect.effect_id == EFFECT_MINING_LASER)).expect("peer rookie mining laser").item_id;
        globals.set("foreign_module", foreign_module).unwrap();
        r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&peer, &foreign]).await.unwrap();
        for call in ["pilots.activate(peer,foreign_module,nil,nil)", "pilots.deactivate(peer,foreign_module)", "pilots.lock_target(peer,1)", "pilots.unlock_target(peer,1)", "pilots.activate(id,foreign_module,nil,nil)", "pilots.deactivate(id,foreign_module)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "foreign: {call}");
        }
        let original = r.world.remove_ball(system, ship).await.unwrap();
        let mut wrong = original.clone();
        wrong.char_id = Some(peer);
        r.world.add_ship(system, wrong).await.unwrap();
        for call in ["pilots.activate(id,module,nil,nil)", "pilots.deactivate(id,module)", "pilots.lock_target(id,1)", "pilots.unlock_target(id,1)"] {
            assert!(lua.load(call).exec_async().await.is_err(), "actor mismatch: {call}");
        }
        r.world.remove_ball(system, ship).await;
        let fit = Fit::load(&r.db, r.sde.clone(), id, ship).await.unwrap();
        let condition = combat::prepare_condition_with_fit(&r.db, ship, &fit).await.unwrap();
        r.world.with(system, move |s| {
            s.add_ship(original);
            s.install_condition(ship, condition.into_condition(), fit);
            assert_eq!(s.pilot_ball(id).map(|ball| ball.id), Some(ship));
            assert!(s.persistent_generation(ship).is_some());
            assert_eq!(s.combat_membership_generation(ship), s.persistent_generation(ship));
            assert!(s.ship_condition(ship).is_some());
        }).await.unwrap();
        // The ore generator spreads centers up to 43 km and clamps rock radii
        // at 25 km. Clear every existing fixed massive sphere by that envelope,
        // plus room for the ship and its 100 m offset from the chosen rock.
        let center = r.world.with(system, move |s| {
            let mut center = s.sample_position(ship)?;
            let ship_radius = f64::from(s.ball(ship)?.radius);
            for ball in s.balls().filter(|ball| !ball.free && ball.massive) {
                center[0] = center[0].max(ball.pos[0] + f64::from(ball.radius) + 43_000.0 + 25_000.0 + ship_radius + 1_000.0);
            }
            Some(center)
        }).await.flatten().expect("undocked ship must remain in space");
        *test_field.lock().unwrap() = Some(ship);
        let rocks = mining::grow_field(&r.db, &r.world, system, ship, center, crate::space::asteroids::Field::Ore, 1.0, false).await.unwrap();
        let (rock, ship_radius) = r.world.with(system, move |s| {
            let rock = rocks.into_iter().map(|id| {
                let ball = s.ball(id).expect("generated asteroid must remain in space");
                (ball.id, ball.pos, ball.radius, ball.type_id)
            }).max_by(|a, b| (a.1[0] + f64::from(a.2)).total_cmp(&(b.1[0] + f64::from(b.2))))
                .expect("generated asteroid field");
            Some((rock, f64::from(s.ball(ship)?.radius)))
        }).await.flatten().expect("ship must remain in space");
        let rock_id = rock.0;
        globals.set("rock", rock_id).unwrap();
        let position = [rock.1[0] + f64::from(rock.2) + ship_radius + 100.0, rock.1[1], rock.1[2]];
        lua.load("pilots.stop(id)").exec_async().await.unwrap();
        let relocation_tick = r.world.with(system, move |s| {
            s.set_ball_position(ship, position, |_, _| true);
            s.current_time()
        }).await.unwrap();
        // The queued action takes effect at the next destiny boundary; the next
        // completed segment must also replace the pre-teleport interpolation.
        let mut tick = relocation_tick;
        for _ in 0..5 {
            if tick >= relocation_tick + 2 { break; }
            assert!(r.world.after(system, 1000).await);
            tick = r.world.with(system, |s| s.current_time()).await.unwrap();
        }
        assert!(tick >= relocation_tick + 2, "relocation did not advance two destiny ticks");
        let fit = Fit::load_in_space(&r.db, &r.world, system, id, ship).await.unwrap();
        let (distance, ship_radius, rock_radius) = r.world.with(system, move |s| {
            let ship_pos = s.sample_position(ship)?;
            let rock_pos = s.sample_position(rock_id)?;
            Some((crate::space::dist(ship_pos, rock_pos), f64::from(s.ball(ship)?.radius), f64::from(s.ball(rock_id)?.radius)))
        }).await.flatten().expect("ship and asteroid must remain in space");
        let lock_gap = distance - rock_radius;
        let mining_gap = lock_gap - ship_radius;
        let lock_range = fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE);
        let mining_range = fit.value(Key::Module(module_id), ATTR_MAX_RANGE);
        assert!(lock_gap <= lock_range && mining_gap <= mining_range,
            "sampled gaps exceed fitted ranges: lock {lock_gap}/{lock_range}, mining {mining_gap}/{mining_range}");
        let signature = r.sde.types.attribute(rock.3, ATTR_SIGNATURE_RADIUS).unwrap_or(f64::from(rock.2));
        let lock_ms = crate::space::lock_time_ms(fit.value(Key::Ship, ATTR_SCAN_RESOLUTION), signature);
        let duration = fit.value(Key::Module(module_id), effect.duration_attribute_id.unwrap());
        assert!(duration >= 1000.0, "rookie SDE cycle must exceed the service minimum");
        let duration = duration as u64;
        lua.load("pilots.lock_target(id,rock)").exec_async().await.unwrap();
        assert!(lua.load("pilots.activate(id,module,rock,nil)").exec_async().await.is_err());
        lua.load("pilots.unlock_target(id,rock)").exec_async().await.unwrap();
        assert!(r.world.after(system, lock_ms + 1100).await);
        assert!(lua.load("pilots.activate(id,module,rock,nil)").exec_async().await.is_err());
        lua.load("pilots.lock_target(id,rock)").exec_async().await.unwrap();
        assert!(r.world.after(system, lock_ms + 1100).await);
        let before = crate::dao::items::loading::load_items(&r.db, ItemQuery::Location(ship)).await.unwrap();
        let cap_before: f64 = lua.load("return pilots.condition(id).capacitor").eval_async().await.unwrap();
        lua.load("pilots.activate(id,module,rock,nil)").exec_async().await.unwrap();
        assert!(r.world.after(system, 1100).await);
        let cap_after: f64 = lua.load("return pilots.condition(id).capacitor").eval_async().await.unwrap();
        assert!(cap_after < cap_before);
        assert!(combat::active_modules().contains(&module_id));
        assert!(r.world.after(system, duration + 2000).await);
        let after = crate::dao::items::loading::load_items(&r.db, ItemQuery::Location(ship)).await.unwrap();
        assert!(after.iter().any(|item| before.iter().all(|old| old.item_id != item.item_id)));
        assert!(!combat::active_modules().contains(&module_id));
        assert_eq!(deliveries(&r.db, id).await, 1);
        lua.load("pilots.activate(id,module,rock,-1)").exec_async().await.unwrap();
        assert!(r.world.after(system, duration + 2000).await);
        assert!(!combat::active_modules().contains(&module_id));
        assert_eq!(deliveries(&r.db, id).await, 2);
        lua.load("pilots.activate(id,module,rock,5)").exec_async().await.unwrap();
        assert!(r.world.after(system, duration * 2 + 2000).await);
        assert!(deliveries(&r.db, id).await >= 4, "repeating effect must deliver at least two cycles");
        assert!(combat::active_modules().contains(&module_id));
        lua.load("pilots.deactivate(id,module)").exec_async().await.unwrap();
        assert!(r.world.after(system, duration + 2000).await);
        assert!(!combat::active_modules().contains(&module_id));
        let stopped_deliveries = deliveries(&r.db, id).await;
        assert!(r.world.after(system, duration + 2000).await);
        assert_eq!(deliveries(&r.db, id).await, stopped_deliveries);
        assert!(!combat::active_modules().contains(&module_id));
        lua.load("pilots.unlock_target(id,rock)").exec_async().await.unwrap();
        assert!(lua.load("pilots.activate(id,module,rock,nil)").exec_async().await.is_err());
    }).await;
    let _ = lifecycle::offline(r.clone(), id).await;
    let saved_field = *field.lock().unwrap();
    if let Some(field) = saved_field {
        let rows = r.db.query("SELECT asteroid_id, system_id FROM game.asteroids WHERE field_id=$1", &[&field]).await.unwrap();
        for row in rows {
            let rock: i64 = row.get("asteroid_id");
            let system: i64 = row.get("system_id");
            let actor = r.world.asteroid_actor(system).expect("fixture asteroid actor");
            let owner = actor.rock_field(rock).await.expect("fixture rock metadata");
            assert_eq!(owner, field);
            r.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND system_id=$2 AND field_id=$3", &[&rock, &system, &field]).await.unwrap();
            assert!(crate::space::asteroids::publish_depletion(&r.world, system, &actor, rock, field, None, None).await);
        }
    }
    r.db.execute("UPDATE game.characters SET user_id=$2 WHERE character_id=$1", &[&peer, &r.pilots.account]).await.unwrap();
    let _ = lifecycle::offline(r.clone(), peer).await;
    fixture::cleanup(&r.db, peer).await;
    r.db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&foreign]).await.unwrap();
    r.db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1", &[&id]).await.unwrap();
    fixture::cleanup(&r.db, id).await;
    fields.restore(&r).await;
    result.unwrap();
}
