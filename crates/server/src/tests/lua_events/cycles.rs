//! Real Lua-host mining cycle acceptance using the actor and committed mining facts.

use std::{sync::{Arc, Mutex}, time::Duration};
use crate::{consts::{dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_CAPACITOR_NEED, ATTR_CAPACITY, ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_RECHARGE_RATE, ATTR_SCAN_RESOLUTION, ATTR_SIGNATURE_RADIUS, EFFECT_MINING_LASER}, FLAG_CARGO}, dao::items::loading::ItemQuery, services::{combat, fit::{Fit, Key}, mining}};

mod banks;
#[path = "cycles/mining.rs"]
mod mining_acceptance;

#[derive(Default)]
struct Keys {
    character: Option<i64>,
    field: Option<i64>,
}

async fn idle(world: &Arc<crate::space::World>, system: i64, module: i64, timeout: u64) {
    tokio::time::timeout(Duration::from_millis(timeout), async {
        while combat::active_modules().contains(&module) {
            assert!(world.after(system, 100).await);
        }
    }).await.expect("module cycle retirement");
}

pub(super) async fn run(ctx: &super::Context) {
    let cursor = ctx.events().await.len();
    let keys = Arc::new(Mutex::new(Keys::default()));
    let saved = keys.clone();
    let r = ctx.resources.clone();
    let lua = ctx.lua.clone();
    let result = tokio::spawn(async move {
        let races = crate::dao::sde::characters::races(&r.db).await.unwrap();
        let bloodline = crate::dao::sde::characters::bloodlines(&r.db).await.unwrap().first().unwrap().bloodline_id;
        let (race, school, station) = races.iter().find_map(|race| {
            let loaded = r.sde.characters.race(race.race_id)?;
            if loaded.skills.is_empty() || loaded.ship_type_id.is_none() { return None; }
            r.sde.characters.schools_for_race(race.race_id as i32).find_map(|school| {
                school.corporation_id?;
                let station = school.starting_stations.iter().filter_map(|id| r.sde.map.station(*id))
                    .find(|station| station.solar_system_id.is_some() && station.constellation_id.is_some() && station.region_id.is_some())?;
                Some((race.race_id, school.school_id, station.station_id))
            })
        }).expect("SDE race with starting ship and school");
        let globals = lua.globals();
        globals.set("cycle_race", race).unwrap();
        globals.set("cycle_bloodline", bloodline).unwrap();
        globals.set("cycle_school", school).unwrap();
        globals.set("cycle_station", station).unwrap();
        globals.set("cycle_name", format!("CyclePilot{}", crate::space::now_ms())).unwrap();
        let id: i64 = lua.load("return pilots.create{name=cycle_name,race_id=cycle_race,bloodline_id=cycle_bloodline,school_id=cycle_school,station_id=cycle_station,gender_id=0,balance=0}").eval_async().await.unwrap();
        saved.lock().unwrap().character = Some(id);
        globals.set("cycle_id", id).unwrap();
        lua.load("pilots.online(cycle_id)").exec_async().await.unwrap();
        let ship: i64 = r.db.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&id]).await.unwrap().get("ship_id");
        let items = crate::services::items::load_items(&r.db, &r.sde, ItemQuery::Location(ship)).await.unwrap();
        let module = items.iter().find(|item| r.sde.types.effects(item.type_id).iter().any(|effect| effect.is_default && effect.effect_id == EFFECT_MINING_LASER))
            .expect("rookie mining laser with SDE default").item_id;
        let hull = crate::services::items::load_items(&r.db, &r.sde, ItemQuery::Id(ship)).await.unwrap().remove(0);
        let docked = Fit::load(&r.db, r.sde.clone(), id, ship).await.unwrap();
        let effect = r.sde.dogma.effect(EFFECT_MINING_LASER).unwrap();
        let need = docked.value(Key::Module(module), effect.discharge_attribute_id.unwrap_or(ATTR_CAPACITOR_NEED));
        let base_capacity = r.sde.types.attribute(hull.type_id, ATTR_CAPACITOR_CAPACITY).unwrap();
        let multiplier = docked.value(Key::Ship, ATTR_CAPACITOR_CAPACITY) / base_capacity;
        assert!(need > 0.0 && multiplier > 0.0);
        assert!(crate::dao::items::dogma::set_override(&r.db, ship, ATTR_CAPACITOR_CAPACITY, need * 1.01 / multiplier).await.unwrap());
        lua.load("pilots.undock(cycle_id)").exec_async().await.unwrap();
        let identity: mlua::Table = lua.load("return pilots.identity(cycle_id)").eval_async().await.unwrap();
        assert_eq!(identity.get::<i64>("ship_id").unwrap(), ship);
        let system: i64 = identity.get("system_id").unwrap();
        globals.set("cycle_module", module).unwrap();
        let center = r.world.with(system, move |s| {
            let mut center = s.sample_position(ship)?;
            let radius = f64::from(s.ball(ship)?.radius);
            for ball in s.balls().filter(|ball| !ball.free && ball.massive) {
                center[0] = center[0].max(ball.pos[0] + f64::from(ball.radius) + 43_000.0 + 25_000.0 + radius + 1_000.0);
            }
            Some(center)
        }).await.flatten().unwrap();
        saved.lock().unwrap().field = Some(ship);
        let rocks = mining::grow_field(&r.db, &r.world, system, ship, center, mining::Field::Ore, 1.0, false).await.unwrap();
        let (rock, ship_radius) = r.world.with(system, move |s| {
            let rock = rocks.into_iter().map(|id| {
                let b = s.ball(id).unwrap();
                (b.id, b.pos, b.radius, b.type_id)
            }).max_by(|a, b| (a.1[0] + f64::from(a.2)).total_cmp(&(b.1[0] + f64::from(b.2))))?;
            Some((rock, f64::from(s.ball(ship)?.radius)))
        }).await.flatten().expect("generated field and ship");
        globals.set("cycle_rock", rock.0).unwrap();
        lua.load("pilots.stop(cycle_id)").exec_async().await.unwrap();
        let position = [rock.1[0] + f64::from(rock.2) + ship_radius + 100.0, rock.1[1], rock.1[2]];
        let tick = r.world.with(system, move |s| { s.set_ball_position(ship, position, |_, _| true); s.current_time() }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while r.world.with(system, |s| s.current_time()).await.unwrap() < tick + 2 {
                assert!(r.world.after(system, 1000).await);
            }
        }).await.expect("relocation");
        let fit = Fit::load_in_space(&r.db, &r.world, system, id, ship).await.unwrap();
        let (gap, sr) = r.world.with(system, move |s| {
            Some((crate::space::dist(s.sample_position(ship)?, s.sample_position(rock.0)?) - f64::from(s.ball(rock.0)?.radius), f64::from(s.ball(ship)?.radius)))
        }).await.flatten().unwrap();
        assert!(gap <= fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE));
        assert!(gap - sr <= fit.value(Key::Module(module), ATTR_MAX_RANGE));
        let signature = r.sde.types.attribute(rock.3, ATTR_SIGNATURE_RADIUS).unwrap_or(f64::from(rock.2));
        let lock_ms = crate::space::lock_time_ms(fit.value(Key::Ship, ATTR_SCAN_RESOLUTION), signature);
        let effect = r.sde.dogma.effect(EFFECT_MINING_LASER).unwrap();
        let duration = fit.value(Key::Module(module), effect.duration_attribute_id.unwrap()) as u64;
        lua.load("pilots.lock_target(cycle_id,cycle_rock)").exec_async().await.unwrap();
        assert!(r.world.after(system, lock_ms + 1100).await);
        let initial: i64 = r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get("quantity");
        let cap = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let tau = fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        let elapsed = duration as f64 / 1000.0;
        let after = cap * (1.0 + (((cap - need).max(0.0) / cap).sqrt() - 1.0) * (-5.0 * elapsed / (tau / 1000.0).max(0.001)).exp()).powi(2);
        assert!(cap >= need && after < need, "fixture cannot fail the repeat: capacity={cap}, need={need}, recharged={after}");
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,2)").exec_async().await.unwrap();
        idle(&r.world, system, module, duration * 2 + 15_000).await;
        assert_eq!(crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap().len(), 1);
        crate::dao::items::dogma::clear_override(&r.db, ship, ATTR_CAPACITOR_CAPACITY).await.unwrap();
        combat::groups_health::inject(&r.db, &r.world, system, ship, f64::MAX).await.unwrap();
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,nil)").exec_async().await.unwrap();
        idle(&r.world, system, module, duration + 15_000).await;
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,2)").exec_async().await.unwrap();
        idle(&r.world, system, module, duration * 2 + 15_000).await;
        let ledger = crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap();
        assert_eq!(ledger.len(), 4);
        assert!(ledger.iter().all(|row| row.type_id == rock.3 && row.solar_system_id == system && row.quantity > 0));
        let mined: i64 = ledger.iter().map(|row| row.quantity).sum();
        let waste: i64 = ledger.iter().map(|row| row.quantity_wasted).sum();
        let remaining: i64 = r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get("quantity");
        let cargo: i64 = r.db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get("n");
        assert_eq!(initial - remaining, mined + waste);
        assert_eq!(cargo, mined);
        let volume = r.sde.types.get(rock.3).unwrap().volume.unwrap_or(1.0).max(0.01);
        let fill = (fit.value(Key::Ship, ATTR_CAPACITY) / volume).ceil() as i32 + 1;
        let stack: i64 = r.db.query_one("SELECT item_id FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get("item_id");
        r.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&stack, &fill]).await.unwrap();
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,nil)").exec_async().await.unwrap();
        idle(&r.world, system, module, duration + 15_000).await;
        assert_eq!(crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap().len(), 4);
        assert_eq!(r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get::<_, i64>("quantity"), remaining);
        r.db.execute("UPDATE game.items SET quantity=$2 WHERE item_id=$1", &[&stack, &(mined as i32)]).await.unwrap();
        assert!(duration > 5_000, "fixture needs time for actor movement during a real cycle");
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,nil)").exec_async().await.unwrap();
        let far = [rock.1[0] + f64::from(rock.2) + ship_radius + fit.value(Key::Module(module), ATTR_MAX_RANGE) + 1_000.0, rock.1[1], rock.1[2]];
        let move_tick = r.world.with(system, move |s| { s.set_ball_position(ship, far, |_, _| true); s.current_time() }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while r.world.with(system, |s| s.current_time()).await.unwrap() < move_tick + 2 {
                assert!(r.world.after(system, 1000).await);
            }
        }).await.expect("out-of-range relocation");
        idle(&r.world, system, module, duration + 15_000).await;
        let return_tick = r.world.with(system, move |s| { s.set_ball_position(ship, position, |_, _| true); s.current_time() }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while r.world.with(system, |s| s.current_time()).await.unwrap() < return_tick + 2 {
                assert!(r.world.after(system, 1000).await);
            }
        }).await.expect("return relocation");
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,nil)").exec_async().await.unwrap();
        let ship_ball = r.world.remove_ball(system, ship).await.unwrap();
        idle(&r.world, system, module, duration + 15_000).await;
        r.world.add_ship(system, ship_ball).await.unwrap();
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,2); pilots.deactivate(cycle_id,cycle_module)").exec_async().await.unwrap();
        let ball = r.world.remove_ball(system, rock.0).await.unwrap();
        idle(&r.world, system, module, duration + 15_000).await;
        r.world.add_ship(system, ball).await.unwrap();
        lua.load("pilots.unlock_target(cycle_id,cycle_rock); pilots.lock_target(cycle_id,cycle_rock)").exec_async().await.unwrap();
        assert!(r.world.after(system, lock_ms + 1100).await);
        combat::groups_health::inject(&r.db, &r.world, system, ship, f64::MAX).await.unwrap();
        lua.load("pilots.activate(cycle_id,cycle_module,cycle_rock,nil)").exec_async().await.unwrap();
        // A successful activation only queues the cycle; wait for its real capacitor payment.
        tokio::time::timeout(Duration::from_millis(duration / 2), async {
            loop {
                let snapshot = combat::groups_health::capacitor_snapshot(&r.db, &r.world, system, ship).await.unwrap().unwrap();
                if snapshot.level < snapshot.capacity - need / 2.0 { break; }
                assert!(combat::active_modules().contains(&module), "cycle retired before capacitor payment");
                tokio::task::yield_now().await;
            }
        }).await.expect("cycle capacitor payment");
        r.world.with(system, move |s| {
            assert_eq!(s.ball(ship).and_then(|ball| ball.char_id), Some(id));
            assert!(combat::active_modules().contains(&module));
            s.remove_ball(rock.0).expect("locked target present during cycle")
        }).await.unwrap();
        idle(&r.world, system, module, duration + 15_000).await;
        assert_eq!(crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap().len(), 4);
        (id, module)
    }).await;
    let (character, field) = { let keys = keys.lock().unwrap(); (keys.character, keys.field) };
    if let Some(id) = character {
        ctx.lua.globals().set("cycle_cleanup_id", id).unwrap();
        let _ = ctx.lua.load("pilots.offline(cycle_cleanup_id)").exec_async().await;
        if let Some(field) = field {
            let rows = ctx.resources.db.query("SELECT asteroid_id FROM game.asteroids WHERE field_id=$1", &[&field]).await.unwrap();
            for row in rows {
                let rock: i64 = row.get("asteroid_id");
                mining::reconcile_depleted(&ctx.resources.db, &ctx.resources.world, rock).await.unwrap();
                ctx.resources.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND field_id=$2", &[&rock, &field]).await.unwrap();
            }
            ctx.resources.db.execute("DELETE FROM game.asteroid_fields WHERE field_id=$1", &[&field]).await.unwrap();
        }
        ctx.resources.db.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1", &[&id]).await.unwrap();
        ctx.resources.db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&id]).await.unwrap();
        ctx.resources.db.execute("DELETE FROM game.items WHERE owner_id=$1", &[&id]).await.unwrap();
        ctx.resources.db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&id]).await.unwrap();
    }
    let (id, module) = result.unwrap();
    ctx.drain().await;
    let events = ctx.events().await;
    let cycles: Vec<_> = events[cursor..].iter().filter(|event| event["kind"] == "ModuleCycleEnded" && event["char_id"] == id).collect();
    assert_eq!(cycles.len(), 7, "{cycles:?}");
    for (event, outcome) in cycles.iter().zip(["completed", "out_of_capacitor", "completed", "completed", "completed", "cargo_full", "target_lost"]) {
        assert_eq!(event["module_item_id"], module);
        assert_eq!(event["effect_id"], i64::from(EFFECT_MINING_LASER));
        assert_eq!(event["outcome"], outcome);
    }
    mining_acceptance::run(ctx).await;
    banks::run(ctx).await;
}
