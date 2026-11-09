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
    let (generation, revision, projection) = f.world.with(f.system, move |s| (s.combat_membership_generation(id).unwrap(), s.combat_fit_revision(id).unwrap(), s.projection_revision(id))).await.unwrap();
    let fit = crate::services::fit::Fit::drone_in_space(&f.db, &f.world, f.system, owner, id).await.ok().unwrap();
    assert!(!f.world.with(f.system, move |s| s.replace_fitted_cache(id, generation.wrapping_add(1), revision, fit, crate::services::fit::Key::Drone(id), Some(projection))).await.unwrap());
    let fit = crate::services::fit::Fit::drone_in_space(&f.db, &f.world, f.system, owner, id).await.ok().unwrap();
    assert!(!f.world.with(f.system, move |s| s.replace_fitted_cache(id, generation, revision.wrapping_add(1), fit, crate::services::fit::Key::Drone(id), Some(projection))).await.unwrap());
    let fit = crate::services::fit::Fit::drone_in_space(&f.db, &f.world, f.system, owner, id).await.ok().unwrap();
    assert!(f.world.with(f.system, move |s| s.replace_fitted_cache(id, generation, revision, fit, crate::services::fit::Key::Drone(id), Some(projection))).await.unwrap());
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
