//! Real fitted ordinary warp capacitor admission against imported ship data.
use super::*;
use infinity_core::consts::{CATEGORY_SHIP, dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, ATTR_WARP_CAPACITOR_NEED}};
use crate::services::fit::{Fit, Key};
use std::sync::Arc;

#[path = "../../combat/groups_health/tests/fixture.rs"]
mod fixture;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn real_fitted_positive_warp_debits_shared_capacitor() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let before = fixture::fingerprint(&db).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|ty| {
        if ty.category_id != Some(CATEGORY_SHIP) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
        let need = fit.value(Key::Ship, ATTR_WARP_CAPACITOR_NEED);
        let capacity = fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        let tau = fit.value(Key::Ship, ATTR_RECHARGE_RATE);
        need.is_finite() && need > 0.0 && capacity.is_finite() && capacity > 0.0 && tau.is_finite() && tau >= 60_000.0
    }).expect("SDE must contain a positively priced ship with usable capacitor").type_id;
    let effect = crate::services::effects::registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_MICROWARPDRIVE).unwrap();
    let discharge = effect.attribute(crate::services::effects::registry::EffectAttribute::Discharge)
        .unwrap_or(infinity_core::consts::dogma::ATTR_CAPACITOR_NEED);
    let duration = effect.attribute(crate::services::effects::registry::EffectAttribute::Duration).unwrap();
    let boost = sde.dogma.attributes_by_name("speedBoostFactor").next().unwrap().attribute_id;
    let module_type = sde.types.iter().find(|ty| ty.published == Some(true)
        && ty.category_id == Some(infinity_core::consts::CATEGORY_MODULE)
        && ty.effects().iter().any(|entry| entry.effect_id == infinity_core::consts::dogma::EFFECT_MICROWARPDRIVE)
        && ty.fit_attribute(infinity_core::consts::dogma::ATTR_SPEED_FACTOR).is_some_and(|value| value > 0.0)
        && ty.fit_attribute(boost).is_some_and(|value| value > 0.0)
        && ty.fit_attribute(duration).is_some_and(|value| (10_000.0..=30_000.0).contains(&value)))
        .expect("published microwarpdrive").type_id;
    let effect_name = sde.dogma.effect(infinity_core::consts::dogma::EFFECT_MICROWARPDRIVE).unwrap().name.clone().unwrap();
    let system = -1;
    let rows = fixture::create(&db, ship_type, system).await.unwrap();
    let module = match crate::dao::items::creation::rookie_ship(&db, module_type, rows.character, rows.ship, infinity_core::consts::FLAG_MED_SLOT_FIRST).await {
        Ok(module) => module,
        Err(error) => {
            fixture::cleanup(&db, rows).await.unwrap();
            panic!("module fixture creation failed: {error}");
        }
    };
    let setup = async {
        let updated = crate::dao::items::combat::set_online(&db, module, rows.ship, rows.character,
            infinity_core::consts::FLAG_MED_SLOT_FIRST, module_type, ship_type, system, 0, true, true).await.map_err(|error| error.to_string())?;
        if updated != 1 { return Err("module online setup refused".to_string()); }
        let unmodified = Fit::load(&db, sde.clone(), rows.character, rows.ship).await.map_err(|error| format!("{error:?}"))?;
        let cap = unmodified.value(Key::Ship, ATTR_CAPACITOR_CAPACITY);
        if !crate::dao::items::dogma::set_override(&db, module, discharge, cap * 0.1).await.map_err(|error| error.to_string())? {
            return Err("module discharge setup refused".to_string());
        }
        Ok::<_, String>(())
    }.await;
    if let Err(error) = setup {
        crate::dao::items::inventory::delete(&db, module).await.unwrap();
        fixture::cleanup(&db, rows).await.unwrap();
        panic!("module fixture setup failed: {error}");
    }
    let world = space::World::new(db.clone(), sde.clone(), 1);
    let w = world.clone();
    let d = db.clone();
    let result = tokio::spawn(async move {
        w.system(system).await.unwrap();
        let row = crate::dao::items::space_ships::FreeShip {
            item_id: rows.ship, type_id: ship_type, owner_id: rows.character, corporation_id: 1,
            alliance_id: -1, pos: [0.0; 3],
        };
        let prepared = balls::piloted(&d, sde, &row, rows.character, 0.0).await.unwrap();
        w.with(system, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let (notify, mut notices) = tokio::sync::mpsc::unbounded_channel();
        w.register_pilot(rows.character, notify.clone());
        let identity = Identity { char_id: rows.character, ship_id: Some(rows.ship), corp_id: 1, station_id: None, system_id: Some(system) };
        crate::services::effects::lifecycle::activate(&identity, &d, &w, w.sde(), Some(system),
            notify, rows.account, module, &effect_name, None, 1, |_| Ok(())).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let (name, args) = notices.recv().await.expect("pilot notification channel closed");
                if name == "OnModuleAttributeChanges" && args.first().and_then(Value::items).is_some_and(|changes| changes.iter().any(|change| {
                    let Some(fields) = change.items() else { return false };
                    fields.get(3).and_then(Value::as_int) == Some(i64::from(infinity_core::consts::dogma::ATTR_CHARGE))
                })) { break; }
            }
        }).await.expect("real module capacitor debit");
        let initial = crate::services::combat::groups_health::capacitor_snapshot(&d, &w, system, rows.ship).await.unwrap().unwrap();
        let accepted = warp_pilot(&d, &w, system, rows.character, rows.ship,
            WarpDestination::Ordinary { point: [infinity_core::consts::AU, 0.0, 0.0], object_radius: None, min_range: 0.0 }, None, None).await.unwrap();
        assert!(accepted);
        w.with(system, move |s| s.command(rows.ship, |ball, _, _| {
            ball.vel = [f64::from(ball.max_vel), 0.0, 0.0];
            Vec::new()
        })).await.unwrap();
        let notice = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let (name, args) = notices.recv().await.expect("pilot notification channel closed");
                if name == "OnModuleAttributeChanges" { break args; }
            }
        }).await.expect("warp debit notice");
        let Value::List(changes) = &notice[0] else { panic!("expected attribute change list") };
        assert!(changes.iter().any(|change| {
            let Some(fields) = change.items() else { return false };
            fields.get(3).and_then(Value::as_int) == Some(i64::from(infinity_core::consts::dogma::ATTR_CHARGE))
                && matches!((fields.get(5), fields.get(6)), (Some(Value::Float(after)), Some(Value::Float(before))) if after < before)
        }));
        let before_inject = crate::services::combat::groups_health::capacitor_snapshot(&d, &w, system, rows.ship).await.unwrap().unwrap();
        let injected = crate::services::combat::groups_health::inject(&d, &w, system, rows.ship, initial.capacity * 0.1).await.unwrap();
        assert!(injected > 0.0);
        let after_inject = crate::services::combat::groups_health::capacitor_snapshot(&d, &w, system, rows.ship).await.unwrap().unwrap();
        assert!(after_inject.level > before_inject.level);
    }).await;
    if let Some(active) = crate::services::combat::state().active.get(&module) {
        active.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    for _ in 0..350 {
        if !crate::services::combat::state().active.contains_key(&module) { break; }
        assert!(world.after(system, 100).await);
    }
    world.remove_ball(system, rows.ship).await;
    world.unregister_pilot_generation(rows.character, None);
    crate::services::combat::forget_ship(rows.ship);
    let flush = world.flush_positions().await;
    drop(world);
    crate::dao::items::inventory::delete(&db, module).await.unwrap();
    fixture::cleanup(&db, rows).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[test]
fn zero_need_never_touches_capacitor_authority_or_notifies() {
    use crate::services::combat::groups_health::{Condition, ShipStats};
    use crate::services::fit::attributes::tests::ship_fit;
    struct Restore(i64, Option<(f64, i64)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::services::combat::groups_health::tests::replace_capacitor_test_entry(self.0, self.1);
        }
    }
    crate::tests::init_config();
    let id = -932_101;
    let old = crate::services::combat::groups_health::tests::replace_capacitor_test_entry(id, None);
    let _restore = Restore(id, old);
    let registry = Arc::new(space::Pilots::default());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register_pilot(789, tx);
    let mut system = space::System::new(0, registry, 0);
    let mut ball = ship();
    ball.id = id;
    ball.char_id = Some(789);
    ball.mode = MODE_STOP;
    ball.vel = [f64::from(ball.max_vel), 0.0, 0.0];
    system.insert(ball);
    let fit = ship_fit(&[(ATTR_WARP_CAPACITOR_NEED, 0.0), (ATTR_CAPACITOR_CAPACITY, 0.0)]);
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    system.install_condition(id, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, fit);
    assert!(system.queue_warp(id, 3000, WarpDestination::Exact([infinity_core::consts::AU, 20.0, 30.0]), None));
    system.tick(1000);
    assert!(system.ball(id).unwrap().is_warping());
    assert!(rx.try_recv().is_err());
    assert!(crate::services::combat::groups_health::tests::replace_capacitor_test_entry(id, None).is_none());
}
