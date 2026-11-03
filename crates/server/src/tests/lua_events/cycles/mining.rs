//! Real-host mining transaction and reconciliation event boundaries.
//! Uses the normal mining lifecycle and the game asteroid, item and ledger tables;
//! it does not replace the server's extraction or observer paths.

use std::{sync::{Arc, Mutex}, time::Duration};
use crate::{consts::{dogma::{ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_SIGNATURE_RADIUS, EFFECT_MINING_LASER}, FLAG_CARGO}, dao::items::loading::ItemQuery, services::{fit::{Fit, Key}, mining}};

#[derive(Default)]
struct Keys {
    character: Option<i64>,
    field: Option<i64>,
    trigger_suffix: Option<i64>,
}

async fn settle(world: &Arc<crate::space::World>, system: i64, tick: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while world.with(system, |s| s.current_time()).await.unwrap() < tick + 2 {
            assert!(world.after(system, 1000).await);
        }
    }).await.expect("actor relocation");
}

pub(super) async fn run(ctx: &super::super::Context) {
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
        globals.set("mining_accept_race", race).unwrap();
        globals.set("mining_accept_bloodline", bloodline).unwrap();
        globals.set("mining_accept_school", school).unwrap();
        globals.set("mining_accept_station", station).unwrap();
        globals.set("mining_accept_name", format!("MiningBoundary{}", crate::space::now_ms())).unwrap();
        let id: i64 = lua.load("return pilots.create{name=mining_accept_name,race_id=mining_accept_race,bloodline_id=mining_accept_bloodline,school_id=mining_accept_school,station_id=mining_accept_station,gender_id=0,balance=0}").eval_async().await.unwrap();
        { let mut keys = saved.lock().unwrap(); keys.character = Some(id); keys.trigger_suffix = Some(id); }
        globals.set("mining_accept_id", id).unwrap();
        lua.load("pilots.online(mining_accept_id)").exec_async().await.unwrap();
        let ship: i64 = r.db.query_one("SELECT ship_id FROM game.characters WHERE character_id=$1", &[&id]).await.unwrap().get("ship_id");
        let items = crate::services::items::load_items(&r.db, &r.sde, ItemQuery::Location(ship)).await.unwrap();
        let module = items.iter().find(|item| r.sde.types.effects(item.type_id).iter().any(|effect| effect.is_default && effect.effect_id == EFFECT_MINING_LASER)).expect("rookie mining laser").item_id;
        globals.set("mining_accept_module", module).unwrap();
        lua.load("pilots.undock(mining_accept_id)").exec_async().await.unwrap();
        let identity: mlua::Table = lua.load("return pilots.identity(mining_accept_id)").eval_async().await.unwrap();
        let system: i64 = identity.get("system_id").unwrap();
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
            let rock = rocks.into_iter().map(|id| { let b = s.ball(id).unwrap(); (b.id, b.pos, b.radius, b.type_id) })
                .max_by(|a, b| (a.1[0] + f64::from(a.2)).total_cmp(&(b.1[0] + f64::from(b.2))))?;
            Some((rock, f64::from(s.ball(ship)?.radius)))
        }).await.flatten().unwrap();
        globals.set("mining_accept_rock", rock.0).unwrap();
        lua.load("pilots.stop(mining_accept_id)").exec_async().await.unwrap();
        let position = [rock.1[0] + f64::from(rock.2) + ship_radius + 100.0, rock.1[1], rock.1[2]];
        let tick = r.world.with(system, move |s| { s.set_ball_position(ship, position, |_, _| true); s.current_time() }).await.unwrap();
        settle(&r.world, system, tick).await;
        let fit = Fit::load_in_space(&r.db, &r.world, system, id, ship).await.unwrap();
        let (gap, sr) = r.world.with(system, move |s| Some((crate::space::dist(s.sample_position(ship)?, s.sample_position(rock.0)?) - f64::from(s.ball(rock.0)?.radius), f64::from(s.ball(ship)?.radius)))).await.flatten().unwrap();
        assert!(gap <= fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE));
        assert!(gap - sr <= fit.value(Key::Module(module), ATTR_MAX_RANGE));
        let signature = r.sde.types.attribute(rock.3, ATTR_SIGNATURE_RADIUS).unwrap_or(f64::from(rock.2));
        let lock_ms = crate::space::lock_time_ms(fit.value(Key::Ship, ATTR_SCAN_RESOLUTION), signature);
        let effect = r.sde.dogma.effect(EFFECT_MINING_LASER).unwrap();
        let duration = fit.value(Key::Module(module), effect.duration_attribute_id.unwrap()) as u64;
        lua.load("pilots.lock_target(mining_accept_id,mining_accept_rock)").exec_async().await.unwrap();
        assert!(r.world.after(system, lock_ms + 1100).await);
        let volume = r.sde.types.get(rock.3).unwrap().volume.unwrap_or(1.0).max(0.01);
        assert!(mining::yield_values(&fit, Key::Module(module))[0] >= volume, "fixture must deterministically deliver at least one unit");

        // The ledger insert is after the rock update and stack write in the same transaction.
        let trigger = format!("mining_rollback_{id}");
        r.db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'mining acceptance rollback'; END $$"), &[]).await.unwrap();
        r.db.execute(&format!("CREATE TRIGGER {trigger} BEFORE INSERT ON game.character_mining_ledger FOR EACH ROW WHEN (NEW.character_id = {id}) EXECUTE FUNCTION game.{trigger}()"), &[]).await.unwrap();
        let before: i64 = r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get("quantity");
        let cargo_before: i64 = r.db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get("n");
        let ledger_before = crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap().len();
        let stacks_before: Vec<(i64, i32)> = r.db.query("SELECT item_id, quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 ORDER BY item_id", &[&id, &ship, &FLAG_CARGO]).await.unwrap().iter().map(|row| (row.get("item_id"), row.get("quantity"))).collect();
        let ledger_rows_before = crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap();
        lua.load("pilots.activate(mining_accept_id,mining_accept_module,mining_accept_rock,nil)").exec_async().await.unwrap();
        super::idle(&r.world, system, module, duration + 15_000).await;
        assert_eq!(r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get::<_, i64>("quantity"), before);
        assert_eq!(r.db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get::<_, i64>("n"), cargo_before);
        assert_eq!(crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap().len(), ledger_before);
        let stacks_after: Vec<(i64, i32)> = r.db.query("SELECT item_id, quantity FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 ORDER BY item_id", &[&id, &ship, &FLAG_CARGO]).await.unwrap().iter().map(|row| (row.get("item_id"), row.get("quantity"))).collect();
        assert_eq!(stacks_after, stacks_before);
        assert_eq!(crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap(), ledger_rows_before);
        r.db.execute(&format!("DROP TRIGGER {trigger} ON game.character_mining_ledger"), &[]).await.unwrap();
        r.db.execute(&format!("DROP FUNCTION game.{trigger}()"), &[]).await.unwrap();
        // Invalidate the fitted module only after the real cycle has started.
        assert!(duration > 5_000, "fixture needs time to invalidate the captured source");
        lua.load("pilots.activate(mining_accept_id,mining_accept_module,mining_accept_rock,nil)").exec_async().await.unwrap();
        assert!(r.world.after(system, 1000).await);
        assert_eq!(r.db.execute("UPDATE game.items SET online=false WHERE item_id=$1 AND online", &[&module]).await.unwrap(), 1);
        super::idle(&r.world, system, module, duration + 15_000).await;
        assert_eq!(r.db.execute("UPDATE game.items SET online=true WHERE item_id=$1 AND NOT online", &[&module]).await.unwrap(), 1);
        assert_eq!(r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get::<_, i64>("quantity"), before);
        assert_eq!(crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap(), ledger_rows_before);

        // Deplete a real rock through the normal cycle; only post-commit deletion fails.
        let trigger = format!("mining_reconcile_{id}");
        r.db.execute(&format!("CREATE FUNCTION game.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'mining acceptance reconcile'; END $$"), &[]).await.unwrap();
        r.db.execute(&format!("CREATE TRIGGER {trigger} BEFORE DELETE ON game.asteroids FOR EACH ROW WHEN (OLD.asteroid_id = {}) EXECUTE FUNCTION game.{trigger}()", rock.0), &[]).await.unwrap();
        r.db.execute("UPDATE game.asteroids SET quantity=1 WHERE asteroid_id=$1", &[&rock.0]).await.unwrap();
        lua.load("pilots.activate(mining_accept_id,mining_accept_module,mining_accept_rock,2)").exec_async().await.unwrap();
        super::idle(&r.world, system, module, duration * 2 + 15_000).await;
        assert!(r.world.after(system, duration + 1000).await);
        assert!(!crate::services::combat::active_modules().contains(&module));
        assert_eq!(r.db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get::<_, i64>("quantity"), 0);
        let ledger = crate::dao::mining_ledger::list(&r.db, id, i64::MIN, i64::MAX).await.unwrap();
        assert_eq!(ledger.len(), ledger_before + 1);
        let row = ledger.last().unwrap();
        assert_eq!(row.type_id, rock.3);
        assert_eq!(row.solar_system_id, system);
        assert_eq!(row.quantity, 1);
        assert_eq!(row.quantity_wasted, 0);
        assert_eq!(r.db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get::<_, i64>("n"), cargo_before + 1);
        (id, module)
    }).await;

    let (character, field, suffix) = { let keys = keys.lock().unwrap(); (keys.character, keys.field, keys.trigger_suffix) };
    // Cleanup is attempted in full even if a previous cleanup operation fails.
    let mut cleanup_errors = Vec::new();
    if let Some(id) = suffix {
        for (name, table) in [(format!("mining_rollback_{id}"), "character_mining_ledger"), (format!("mining_reconcile_{id}"), "asteroids")] {
            if let Err(e) = ctx.resources.db.execute(&format!("DROP TRIGGER IF EXISTS {name} ON game.{table}"), &[]).await { cleanup_errors.push(e.to_string()); }
            if let Err(e) = ctx.resources.db.execute(&format!("DROP FUNCTION IF EXISTS game.{name}()"), &[]).await { cleanup_errors.push(e.to_string()); }
        }
    }
    if let Some(id) = character {
        ctx.lua.globals().set("mining_accept_cleanup", id).unwrap();
        let _ = ctx.lua.load("pilots.offline(mining_accept_cleanup)").exec_async().await;
        if let Some(field) = field {
            match ctx.resources.db.query("SELECT asteroid_id FROM game.asteroids WHERE field_id=$1", &[&field]).await {
                Ok(rows) => for row in rows {
                    let rock: i64 = row.get("asteroid_id");
                    if let Err(e) = mining::reconcile_depleted(&ctx.resources.db, &ctx.resources.world, rock).await { cleanup_errors.push(format!("{e:?}")); }
                    if let Err(e) = ctx.resources.db.execute("DELETE FROM game.asteroids WHERE asteroid_id=$1 AND field_id=$2", &[&rock, &field]).await { cleanup_errors.push(e.to_string()); }
                },
                Err(e) => cleanup_errors.push(e.to_string()),
            }
        }
        for sql in ["DELETE FROM game.character_mining_ledger WHERE character_id=$1", "DELETE FROM game.skills WHERE character_id=$1", "DELETE FROM game.items WHERE owner_id=$1", "DELETE FROM game.characters WHERE character_id=$1"] {
            if let Err(e) = ctx.resources.db.execute(sql, &[&id]).await { cleanup_errors.push(e.to_string()); }
        }
    }
    assert!(cleanup_errors.is_empty(), "cleanup failed: {cleanup_errors:?}");
    let (id, module) = result.unwrap();
    ctx.drain().await;
    let events = ctx.events().await;
    let cycles: Vec<_> = events[cursor..].iter().filter(|event| event["kind"] == "ModuleCycleEnded" && event["char_id"] == id).collect();
    assert_eq!(cycles.len(), 1, "rollback must emit nothing; committed depletion emits once: {cycles:?}");
    assert_eq!(cycles[0]["module_item_id"], module);
    assert_eq!(cycles[0]["effect_id"], EFFECT_MINING_LASER);
    assert_eq!(cycles[0]["outcome"], "completed");
}
