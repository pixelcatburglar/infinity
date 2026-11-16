use super::*;

fn pair(id: i64, count: i64) -> Value { Value::tuple([Value::Int(id), Value::Int(count)]) }

#[test]
fn launch_input_is_exact_and_rejects_duplicate_or_invalid_counts() {
    let valid = [Value::list([pair(10, 2), pair(11, 1)]), Value::Int(99), Value::Bool(false)];
    assert!(matches!(parse(&valid), Ok((rows, Some(99))) if rows == vec![(10, 2), (11, 1)]));
    assert!(matches!(parse(&[Value::list([pair(10, 1)]), Value::None, Value::Bool(false)]), Ok((rows, None)) if rows == vec![(10, 1)]));
    assert!(parse(&[Value::list([pair(10, 1)]), Value::Int(0), Value::Bool(false)]).is_err());
    for rows in [vec![pair(10, 0)], vec![pair(10, -1)], vec![pair(10, 1), pair(10, 1)], vec![Value::Int(10)]] {
        assert!(parse(&[Value::list(rows), Value::Int(99), Value::Bool(false)]).is_err());
    }
    assert!(parse(&[Value::list([pair(10, 1)]), Value::Int(99)]).is_err());
}

#[test]
fn launch_count_gate_uses_effective_character_limit() {
    assert!(check_count(3, 3.0).is_ok());
    assert!(check_count(4, 3.0).is_err());
    assert!(check_count(1, f64::NAN).is_err());
    assert!(check_count(0, 3.0).is_err());
}

#[test]
fn user_failures_keep_named_error_and_dictionary() {
    let value = error_value(crate::services::user_error("Example", vec![("key".into(), Value::Int(4))])).ok().unwrap();
    assert_eq!(value, Value::list([Value::tuple([Value::str("Example"), Value::Dict(vec![(Value::str("key"), Value::Int(4))])])]));
}

use crate::services::drones::tests::fixture::Fixture;
use infinity_core::consts::dogma::{ATTR_DRONE_BANDWIDTH_USED, ATTR_MAX_VELOCITY};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn stale_reservation_and_dropped_ack_keep_one_positioned_drone() {
    let f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
    let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
    let id = crate::dao::items::creation::positioned(&f.db, ty, f.owner, f.system, [0.0; 3]).await.unwrap();
    for (attribute, value) in [(infinity_core::consts::dogma::ATTR_SHIELD_CAPACITY, 1000.0),
        (infinity_core::consts::dogma::ATTR_SHIELD_EM_DAMAGE_RESONANCE, 1.0),
        (infinity_core::consts::dogma::ATTR_SHIELD_RECHARGE_RATE, 1e30)] {
        f.set(id, attribute, value).await;
    }
    let entries = super::super::restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let effects = super::super::super::worker::effects(f.db.clone(), std::sync::Arc::downgrade(&f.world), f.system);
    let owner = f.owner; let ship = f.ship;
    let ack = f.world.with(f.system, move |s| s.complete_drone_launch(0, owner, ship, entries, effects)).await.unwrap().unwrap();
    drop(ack);
    assert!(f.world.with(f.system, move |s| s.balls().filter(|b| b.id == id).count() == 1 && s.drone(id).is_some_and(|d| d.owner == owner && d.controller.is_none())).await.unwrap());
    let stale = super::super::restore_specs(&f.db, f.sde.clone(), f.system).await.ok().unwrap();
    let outcome = f.world.with(f.system, move |s| {
        let generation = s.combat_membership_generation(ship).unwrap();
        let facts = s.prepare_combat_source(ship, None, None, None, crate::lua::events::resources::FactOrigin::Natural,
            generation, 1, 1, |_| true).unwrap();
        s.apply_damage(id, [1.0, 0.0, 0.0, 0.0],
            crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false)
    }).await.unwrap().unwrap().unwrap();
    assert!(outcome.dealt > 0.0 && !outcome.lethal);
    let before = f.world.with(f.system, move |s| s.combat_condition_snapshot(id).unwrap()).await.unwrap();
    let effects = super::super::super::worker::effects(f.db.clone(), std::sync::Arc::downgrade(&f.world), f.system);
    f.world.with(f.system, move |s| for prepared in stale { s.restore_drone(prepared, effects.clone()); }).await.unwrap();
    let after = f.world.with(f.system, move |s| s.combat_condition_snapshot(id).unwrap()).await.unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.condition.now, before.condition.now);
    let rebased = f.world.with(f.system, move |s| s.combat_condition_snapshot(id).unwrap()).await.unwrap();
    assert_eq!(rebased.condition.now[1..], before.condition.now[1..]);
    assert!((rebased.condition.now[0] - before.condition.now[0]).abs() < 1e-6);
    assert_eq!(f.db.query_one("SELECT count(*) FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND singleton AND x IS NOT NULL", &[&id, &owner, &f.system]).await.unwrap().get::<_, i64>("count"), 1);
    let restored = crate::space::World::new(f.db.clone(), f.sde.clone(), 1);
    restored.system(f.system).await.unwrap();
    assert!(restored.with(f.system, move |s| s.balls().filter(|b| b.id == id).count() == 1 && s.drone(id).is_some_and(|d| d.owner == owner && d.controller.is_none())).await.unwrap());
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn cold_system_loads_with_borrowed_market_guard_and_without_one() {
    let f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let market = crate::services::market::lock().await;
        tokio::time::timeout(std::time::Duration::from_secs(10), f.world.system_locked(f.system + 1, &market))
            .await.expect("market-held cold load timed out").unwrap();
        drop(market);
        tokio::time::timeout(std::time::Duration::from_secs(10), f.world.system(f.system + 2))
            .await.expect("outer cold load timed out").unwrap();
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn retry_keeps_frozen_singleton_projection() {
    let f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
        f.train_drone(ty).await;
        let source = f.drone(ty, 1, true).await;
        f.set(source, ATTR_MAX_VELOCITY, 777.0).await;
        let _market = crate::services::market::lock().await;
        let ship = f.ship;
        let ship_projections = f.world.with(f.system, move |s| s.projections_on(ship)).await.unwrap();
        let mut fit = Fit::drone(&f.db, f.sde.clone(), f.owner, source).await.unwrap();
        fit.replace_projections_for(Key::Ship, ship_projections.clone(), None);
        let frozen_speed = fit.value(Key::Drone(source), ATTR_MAX_VELOCITY);
        let frozen = std::sync::Arc::new(fit);
        let expected = crate::services::items::load_items(&f.db, &f.sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
        let input = LaunchInput { sde: f.sde.clone(), expected, owner: f.owner, ship, system: f.system, count: 1,
            pos: [0.0; 3], affiliation: (1, -1), ship_projections, original_fit: Some(std::sync::Arc::downgrade(&frozen)) };
        let (seen, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
        let dispatches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let winner_notices = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (counter, winner) = (dispatches.clone(), winner_notices.clone());
        let drift = 2.0 * frozen_speed;
        let mut attempt = 0;
        let (ids, entries) = f.db.transaction(move |tx, outbox: &mut Outbox| {
            attempt += 1;
            let fail = attempt == 1;
            let input = input.clone();
            let seen = seen.clone();
            Box::pin(async move {
                crate::dao::items::dogma::set_override(tx, source, ATTR_MAX_VELOCITY, drift).await.map_err(db_err)?;
                let prepared = launch_attempt(tx, outbox, input).await?;
                let original = prepared.original.as_ref().expect("singleton launch keeps its original drone");
                let projection = (original.ball.max_vel, original.ball.mass, original.ball.agility, original.ball.signature_radius,
                    original.spec.bandwidth, original.spec.control_range, original.spec.duration_ms, original.condition.scalars(), original.npc_ignored);
                let mut queued = 0;
                let child = std::mem::take(outbox);
                outbox.append_routed(child, |notice| { queued += 1; notice });
                seen.send((prepared.ids.clone(), projection, queued)).unwrap();
                if fail { tx.batch_execute("DO $$ BEGIN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END $$").await.map_err(db_err)?; }
                Ok(prepared)
            })
        }, move |prepared: PreparedLaunch, outbox| async move {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let finished = prepared.finish(Some(frozen))?;
            winner.store(outbox.into_entries().iter().filter(|e| matches!(e, crate::db::outbox::Effect::Notification(_))).count(), std::sync::atomic::Ordering::Relaxed);
            Ok::<_, CallError>(finished)
        }).await.unwrap();
        assert_eq!(ids, vec![source]);
        assert_eq!(entries.len(), 1);
        let observations: Vec<_> = std::iter::from_fn(|| seen_rx.try_recv().ok()).collect();
        let [first, second] = observations.as_slice() else { panic!("expected two candidate attempts") };
        assert_eq!(first, second);
        assert_eq!(first.0, vec![source]);
        let (max_vel, ..) = first.1;
        assert_eq!(max_vel, frozen_speed as f32);
        assert_eq!(first.2, 1);
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(winner_notices.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=$1 AND owner_id=$2 AND location_id=$3 AND flag_id=0 AND singleton AND quantity=1", &[&source, &f.owner, &f.system]).await.unwrap().get::<_, i64>("n"), 1);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn launch_commit_retry_publishes_each_drone_once() {
    let mut f = Fixture::new().await;
    let names = format!("launch_retry_{}", f.owner);
    let mut created = 0;
    let result = crate::tests::catch_scenario(async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)).unwrap().type_id;
        f.train_drone(ty).await;
        let source = f.drone(ty, 1, true).await;
        let fit = Fit::drone(&f.db, f.sde.clone(), f.owner, source).await.unwrap();
        f.set(f.ship, ATTR_DRONE_BANDWIDTH, 4.0 * fit.value(Key::Drone(source), ATTR_DRONE_BANDWIDTH_USED)).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(f.owner, tx);
        f.db.execute(&format!("CREATE SEQUENCE {names}_seq"), &[]).await.unwrap();
        created = 1;
        f.db.execute(&format!("CREATE FUNCTION {names}_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('{names}_seq') = 1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NULL; END $$"), &[]).await.unwrap();
        created = 2;
        f.db.execute(&format!("CREATE CONSTRAINT TRIGGER {names}_trg AFTER UPDATE ON game.items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (OLD.item_id={source} AND OLD.location_id={} AND NEW.location_id={}) EXECUTE FUNCTION {names}_fn()", f.ship, f.system), &[]).await.unwrap();
        created = 3;
        let Value::Dict(launched) = f.call("ship", "LaunchDrones", &[Value::list([pair(source, 1)]), Value::None, Value::Bool(false)]).await.ok().unwrap() else { panic!("singleton launch mapping") };
        assert_eq!(launched, vec![(Value::Int(source), Value::list([Value::Int(source)]))]);
        assert_eq!(f.db.query_one(&format!("SELECT last_value FROM {names}_seq"), &[]).await.unwrap().get::<_, i64>("last_value"), 2);
        let row = f.db.query_one("SELECT location_id, flag_id, quantity, singleton FROM game.items WHERE item_id=$1", &[&source]).await.unwrap();
        assert!(row.get::<_, i64>("location_id") == f.system && row.get::<_, i32>("flag_id") == 0 && row.get::<_, i64>("quantity") == 1 && row.get::<_, bool>("singleton"));
        let owner = f.owner; let ship = f.ship;
        assert!(f.world.with(f.system, move |s| s.drones().count() == 1 && s.drone(source).is_some_and(|d| d.owner == owner && d.controller == Some(ship))).await.unwrap());
        let stack = f.drone(ty, 3, false).await;
        let Value::Dict(launched) = f.call("ship", "LaunchDrones", &[Value::list([pair(stack, 2)]), Value::None, Value::Bool(false)]).await.ok().unwrap() else { panic!("stack launch mapping") };
        let Value::List(values) = &launched[0].1 else { panic!("stack launch failed: {:?}", launched[0].1) };
        let ids: Vec<i64> = values.iter().map(|v| v.as_int().unwrap()).collect();
        assert_eq!(launched[0].0, Value::Int(stack));
        assert!(ids.len() == 2 && ids[0] != ids[1] && ids.iter().all(|&id| id != source && id != stack));
        assert_eq!(f.db.query_one("SELECT quantity FROM game.items WHERE item_id=$1 AND location_id=$2", &[&stack, &f.ship]).await.unwrap().get::<_, i64>("quantity"), 1);
        assert_eq!(f.db.query_one("SELECT count(*) AS n FROM game.items WHERE item_id=ANY($1) AND location_id=$2 AND singleton AND quantity=1", &[&ids, &f.system]).await.unwrap().get::<_, i64>("n"), 2);
        let all = [vec![source], ids.clone()].concat();
        assert!(f.world.with(f.system, move |s| s.drones().count() == 3 && all.iter().all(|&id| s.drone(id).is_some())).await.unwrap());
        let mut notices = Vec::new();
        let mut states = std::collections::BTreeMap::new();
        while let Ok((event, args)) = rx.try_recv() {
            if event == "OnDroneStateChange" { *states.entry(args.first().and_then(Value::as_int).unwrap()).or_insert(0) += 1; }
            notices.push((event, args));
        }
        assert_eq!(states, std::collections::BTreeMap::from([(source, 1), (ids[0], 1), (ids[1], 1)]));
        let descriptor = crate::services::item_descriptor();
        let mut item_changes = Vec::new();
        for (event, args) in &notices {
            if event != "OnItemChange" { continue; }
            let item = load_items(&f.db, &f.sde, ItemQuery::Id(descriptor.get(&args[0], "itemID").unwrap().as_int().unwrap())).await.unwrap().pop().unwrap();
            assert!(args[0] == item.row() && args[2] == Value::None);
            item_changes.push((item.item_id, args[1].clone()));
        }
        let expected: Vec<(i64, Value)> = [(source, Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(1)),
            (Value::Int(IX_LOCATION_ID), Value::Int(f.ship)), (Value::Int(IX_FLAG), Value::Int(i64::from(FLAG_DRONE_BAY)))])),
            (stack, Value::Dict(vec![(Value::Int(IX_STACK_SIZE), Value::Int(3))]))].into_iter()
            .chain(ids.iter().map(|&id| (id, Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(0))])))).collect();
        assert_eq!(item_changes, expected);
    }).await;
    if created >= 3 { f.db.execute(&format!("DROP TRIGGER {names}_trg ON game.items"), &[]).await.unwrap(); }
    if created >= 2 { f.db.execute(&format!("DROP FUNCTION {names}_fn()"), &[]).await.unwrap(); }
    if created >= 1 { f.db.execute(&format!("DROP SEQUENCE {names}_seq"), &[]).await.unwrap(); }
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
