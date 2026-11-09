//! Native simulated-pilot mining through the durable daily-goal consumer.
//! Uses real SDE, actor cycles, the mining ledger and the retained Lua package;
//! this does not exercise an authenticated gateway session or client UI.
use super::*;
use infinity_core::{
    consts::{dogma::{ATTR_MAX_RANGE, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, ATTR_SIGNATURE_RADIUS, EFFECT_MINING_LASER}, FLAG_CARGO},
};
use crate::{
    dao::{characters::persistence, daily_goals::instances, items::loading::ItemQuery, mining_ledger, progress as journal},
    lua::events::{GameEvent, resources::FactOrigin},
    scheduler::{self, Job},
    services::{daily_goals::assignment, fit::{Fit, Key}, mining},
};

const CHILD: &str = "INFINITY_DAILY_NATIVE_MINING_CHILD";
const NAME: &str = "lua::host::pilots::tests::daily_goals::native_mining_advances_daily_goal";

fn job(kind: &str) -> Job {
    Job { id: kind.into(), kind: kind.into(), due_at: crate::client::filetime_now(), payload: "{}".into() }
}

#[tokio::test]
#[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
async fn native_mining_advances_daily_goal() {
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--ignored", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(status.success(), "native mining child failed");
        return;
    }
    let root = std::env::temp_dir().join(format!("daily-native-mining-{}", crate::handshake::random_u64()));
    let mut resources = None;
    let mut character = None;
    let mut field = None;
    let scenario = crate::tests::catch_scenario(async {
        let (db, daily) = crate::lua::tests::daily_goals::payments::current_environment(&root).await;
        let r = crate::lua::HOST.get().expect("daily Host initialized").resources.clone();
        resources = Some(r.clone());
        let (race, bloodline, school, station) = fixture::creation_inputs(&r).await;
        field = Some(fixture::FieldSnapshot::capture(&r, station).await);
        let lua = Lua::new();
        register(&lua, &lua.globals(), &r).unwrap();
        let globals = lua.globals();
        globals.set("race", race).unwrap();
        globals.set("bloodline", bloodline).unwrap();
        globals.set("school", school).unwrap();
        globals.set("station", station).unwrap();
        globals.set("pilot_name", format!("DailyMiner{}", crate::space::now_ms())).unwrap();
        let id: i64 = lua.load("return pilots.create{name=pilot_name,race_id=race,bloodline_id=bloodline,school_id=school,station_id=station,gender_id=0,balance=0}").eval_async().await.unwrap();
        character = Some(id);
        globals.set("id", id).unwrap();
        let owner = persistence::user_for_character(&db, id).await.unwrap().expect("persisted pilot owner");
        crate::dao::accounts::subscriptions::by_user(&db, owner).await.expect("simulated account requires normal clone authority");
        let assigned = assignment::ensure_at(&daily, owner, id, crate::client::filetime_now(), false).await.unwrap();
        let goals = instances::period(&db, id, "day", assigned.bounds.day_start).await.unwrap();
        let goal = goals.into_iter().find(|g| {
            let snapshot: serde_json::Value = serde_json::from_str(&g.definition).unwrap();
            snapshot["template"]["key"] == "mining"
        }).expect("current package assigns MineOre");
        assert_eq!(goal.progress, 0);
        assert!(!goal.paid);
        lua.load("pilots.online(id); pilots.undock(id)").exec_async().await.unwrap();
        assert_eq!(r.world.online_session(id).expect("simulated online session").user_id, None);
        let identity: Table = lua.load("return pilots.identity(id)").eval_async().await.unwrap();
        let ship: i64 = identity.get("ship_id").unwrap();
        let system: i64 = identity.get("system_id").unwrap();
        let items = crate::services::items::load_items(&db, &r.sde, ItemQuery::Location(ship)).await.unwrap();
        let module = items.iter().find(|item| r.sde.types.effects(item.type_id).iter().any(|effect| effect.is_default && effect.effect_id == EFFECT_MINING_LASER)).expect("rookie mining laser from SDE").item_id;
        globals.set("module", module).unwrap();
        let center = r.world.with(system, move |s| {
            let mut center = s.sample_position(ship)?;
            let radius = f64::from(s.ball(ship)?.radius);
            for ball in s.balls().filter(|ball| !ball.free && ball.massive) {
                center[0] = center[0].max(ball.pos[0] + f64::from(ball.radius) + 43_000.0 + 25_000.0 + radius + 1_000.0);
            }
            Some(center)
        }).await.flatten().expect("undocked ship in actor");
        let rocks = mining::grow_field(&db, &r.world, system, ship, center, crate::space::asteroids::Field::Ore, 1.0, false).await.unwrap();
        let (rock, ship_radius) = r.world.with(system, move |s| {
            let rock = rocks.into_iter().map(|id| { let b = s.ball(id).expect("generated rock"); (b.id, b.pos, b.radius, b.type_id) })
                .max_by(|a, b| (a.1[0] + f64::from(a.2)).total_cmp(&(b.1[0] + f64::from(b.2))))?;
            Some((rock, f64::from(s.ball(ship)?.radius)))
        }).await.flatten().expect("ore field and ship");
        globals.set("rock", rock.0).unwrap();
        lua.load("pilots.stop(id)").exec_async().await.unwrap();
        let position = [rock.1[0] + f64::from(rock.2) + ship_radius + 100.0, rock.1[1], rock.1[2]];
        let tick = r.world.with(system, move |s| { s.set_ball_position(ship, position, |_, _| true); s.current_time() }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while r.world.with(system, |s| s.current_time()).await.unwrap() < tick + 2 {
                assert!(r.world.after(system, 1000).await);
            }
        }).await.expect("actor relocation");
        let fit = Fit::load_in_space(&db, &r.world, system, id, ship).await.unwrap();
        let (gap, radius) = r.world.with(system, move |s| Some((crate::space::dist(s.sample_position(ship)?, s.sample_position(rock.0)?) - f64::from(s.ball(rock.0)?.radius), f64::from(s.ball(ship)?.radius)))).await.flatten().unwrap();
        assert!(gap <= fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE));
        assert!(gap - radius <= fit.value(Key::Module(module), ATTR_MAX_RANGE));
        let signature = r.sde.types.attribute(rock.3, ATTR_SIGNATURE_RADIUS).unwrap_or(f64::from(rock.2));
        let lock_ms = crate::space::lock_time_ms(fit.value(Key::Ship, ATTR_SCAN_RESOLUTION), signature);
        let effect = r.sde.dogma.effect(EFFECT_MINING_LASER).expect("mining effect in SDE");
        let duration = fit.value(Key::Module(module), effect.duration_attribute_id.expect("mining duration attribute")) as u64;
        assert!(duration >= 1000, "usable mining cycle duration");
        let cargo_before: i64 = db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get("n");
        let rock_before: i64 = db.query_one("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().get("quantity");
        lua.load("pilots.lock_target(id,rock)").exec_async().await.unwrap();
        assert!(r.world.after(system, lock_ms + 1100).await);
        lua.load("pilots.activate(id,module,rock,nil)").exec_async().await.unwrap();
        assert!(r.world.after(system, duration + 2000).await);
        let ledger = mining_ledger::list(&db, id, i64::MIN, i64::MAX).await.unwrap();
        assert_eq!(ledger.len(), 1, "one real mining cycle");
        let log = &ledger[0];
        assert_eq!(log.type_id, rock.3);
        assert_eq!(log.solar_system_id, system);
        assert!(log.quantity > 0);
        let cargo_after: i64 = db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&id, &ship, &rock.3, &FLAG_CARGO]).await.unwrap().get("n");
        assert_eq!(cargo_after - cargo_before, log.quantity);
        let remaining = db.query_opt("SELECT quantity FROM game.asteroids WHERE asteroid_id=$1", &[&rock.0]).await.unwrap().map(|row| row.get::<_, i64>("quantity")).unwrap_or(0);
        assert_eq!(rock_before - remaining, log.quantity + log.quantity_wasted);
        mining::progress::handle(db.clone(), r.world.clone(), job(scheduler::MINING_PROGRESS)).await.unwrap();
        let source = format!("OreMined:{}", log.log_id);
        let event = journal::source(&db, "business", &source).await.unwrap().expect("canonical mining journal");
        assert_eq!(event.subject_character_id, Some(id));
        assert_eq!(event.origin, FactOrigin::Natural.as_str());
        match &event.event {
            GameEvent::OreMined { char_id, ore_type, context, slices } => {
                assert_eq!((*char_id, *ore_type), (id, log.type_id));
                assert_eq!(context.origin, FactOrigin::Natural);
                assert_eq!(slices.len(), 1);
                assert_eq!((slices[0].log_id, slices[0].occurred_at, slices[0].quantity), (log.log_id, log.event_time, log.quantity));
            }
            _ => panic!("expected canonical OreMined"),
        }
        let key = crate::progress::DeliveryKey { consumer_key: crate::services::daily_goals::progress::registration().consumer_key.into(), event_id: event.event_id.clone() };
        crate::progress::drain(db.clone(), r.world.clone(), job(crate::progress::DRAIN_KIND)).await.unwrap();
        assert!(matches!(journal::delivery(&db, &key).await.unwrap(), journal::DeliveryState::Applied));
        let after = instances::get(&db, id, &goal.goal_id).await.unwrap().unwrap();
        assert_eq!(after.progress, goal.target.min(goal.progress + u64::try_from(log.quantity).unwrap()));
        assert!(after.progress > goal.progress);
        assert!(!after.paid);
        let audits: i64 = db.query_one("SELECT count(*) AS n FROM game.daily_goal_admin_audit WHERE subject_id=$1 OR actor_id=$1", &[&id]).await.unwrap().get("n");
        assert_eq!(audits, 0);
        mining::progress::handle(db.clone(), r.world.clone(), job(scheduler::MINING_PROGRESS)).await.unwrap();
        crate::progress::drain(db.clone(), r.world.clone(), job(crate::progress::DRAIN_KIND)).await.unwrap();
        assert_eq!(instances::get(&db, id, &goal.goal_id).await.unwrap().unwrap(), after);
        assert!(matches!(journal::delivery(&db, &key).await.unwrap(), journal::DeliveryState::Applied));
    }).await;
    let offline = crate::tests::catch_scenario(async {
        if let (Some(r), Some(id)) = (&resources, character) {
            if r.world.is_online(id) { lifecycle::offline(r.clone(), id).await.expect("offline pilot"); }
        }
    }).await;
    let cleanup = crate::tests::catch_scenario(async {
        if let (Some(r), Some(id)) = (&resources, character) {
            assert!(offline.is_ok(), "refusing to remove an online pilot");
            mining::progress::handle(r.db.clone(), r.world.clone(), job(scheduler::MINING_PROGRESS)).await.unwrap();
            for _ in 0..64 {
                let pending = journal::pending_for_character(&r.db, crate::services::daily_goals::progress::registration().consumer_key, id, 1).await.unwrap();
                if pending.is_empty() { break; }
                crate::progress::drain(r.db.clone(), r.world.clone(), job(crate::progress::DRAIN_KIND)).await.unwrap();
            }
            assert!(journal::pending_for_character(&r.db, crate::services::daily_goals::progress::registration().consumer_key, id, 1).await.unwrap().is_empty(), "owned deliveries must be drained before cleanup");
            r.db.transaction(|tx, _: &mut ()| Box::pin(async move {
                tx.execute("DELETE FROM game.daily_goal_admin_audit WHERE subject_id=$1 OR actor_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.daily_goal_payments WHERE character_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&id]).await?;
                tx.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.character_mining_ledger WHERE character_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&id]).await?;
                tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&id]).await?;
                Ok::<(), crate::db::Error>(())
            }), |(), ()| async { Ok(()) }).await.unwrap();
        }
    }).await;
    let restore = crate::tests::catch_scenario(async {
        if let (Some(r), Some(snapshot)) = (&resources, field) { snapshot.restore(r).await; }
    }).await;
    let removed = std::fs::remove_dir_all(&root);
    if let Err(panic) = scenario { std::panic::resume_unwind(panic); }
    if let Err(panic) = offline { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
    if let Err(panic) = restore { std::panic::resume_unwind(panic); }
    removed.unwrap();
}
