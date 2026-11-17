//! Transactional salvage through the fitted effect, persistent inventory and system actor.
use super::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use infinity_core::{consts::{dogma::ATTR_CAPACITOR_NEED, dogma::ATTR_CAPACITY, dogma::ATTR_MAX_RANGE, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_SALVAGING, FLAG_CARGO, FLAG_HI_SLOT_FIRST, GROUP_CARGO_CONTAINER, GROUP_WRECK}};
use infinity_core::consts::dogma::{ATTR_CPU, ATTR_CPU_LOAD, ATTR_POWER, ATTR_POWER_LOAD};
use crate::{dao::items::{creation, dogma, loading::{self, ItemQuery}}, services::{combat::{self, ActiveModule, Ctx, targeting_charges}, effects::{lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module}, registry, salvage}, fit::{Fit, Key}}, space::World};
use marshal::Value;
use std::{sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}}, time::Duration};

// SDE dogma accessDifficulty and accessDifficultyBonus; invGroups Salvaged Materials.
const DIFFICULTY: i32 = 901;
const BONUS: i32 = 902;
const MATERIALS: i32 = 754;
const SYSTEM: i64 = 1;

fn cycle(cx: &Ctx, module: i64, kind: i32, target: i64, effect: registry::Effect, generation: Arc<AtomicBool>) -> Cycle {
    Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: kind, flag: FLAG_HI_SLOT_FIRST }], effect,
        target: Some(target), repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module, generation, armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
        source_membership: None, target_type: None, inputs: Default::default() }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn salvage_preserves_loot_refuses_full_cargo_and_claims_once() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.capacity.is_some_and(|v| v > 0.0) && t.fit_attribute(ATTR_CAPACITY).is_some_and(|v| v > 0.0)).expect("cargo ship").type_id;
    let module = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == EFFECT_SALVAGING)).expect("salvager").type_id;
    let power = sde.types.attribute(module, ATTR_POWER).expect("selected salvager power usage");
    let cpu = sde.types.attribute(module, ATTR_CPU).expect("selected salvager CPU usage");
    let wreck = sde.types.iter().find(|t| t.group_id == Some(GROUP_WRECK) && t.attribute(DIFFICULTY).is_some_and(|v| v >= -200.0)).expect("wreck with difficulty").type_id;
    let loot_type = sde.types.iter().find(|t| t.group_id == Some(MATERIALS) && t.published == Some(true) && t.volume.is_some_and(|v| v > 0.0)).expect("published material").type_id;
    assert!(sde.types.group_types(MATERIALS).iter().filter_map(|id| sde.types.get(*id)).filter(|t| t.published == Some(true)).all(|t| t.volume.is_some_and(|v| v > 0.0)));
    assert_eq!(crate::config::game().salvage.salvage_material_stacks, 1);
    assert_eq!(crate::config::game().salvage.salvage_quantity_min, 1);
    assert_eq!(crate::config::game().salvage.salvage_quantity_max, 3);
    for unlimited in [false, true] {
    // Finite keeps the one-million override path; Unlimited reuses the same
    // scenario with the persisted mode on a zero finite base.
    let mode_capacity = if unlimited { 0.0 } else { 1000000.0 };
    let effect = registry::resolve(&sde, EFFECT_SALVAGING).unwrap();
    let mut f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: module, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let required = crate::services::skills::requirements::requirements(&sde, module).collect::<Vec<_>>();
    let (mut gm, _peer) = crate::services::tests::client(db.clone(), f.character).await;
    crate::services::skills::gm::apply(&mut gm, f.character,
        crate::services::skills::gm::Change::Set(required)).await.unwrap();
    let world = World::new(fixture::cycle_observer(db.clone()), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let sde = sde.clone();
    let target_id = Arc::new(AtomicI64::new(0));
    let scenario_target = target_id.clone();
    let result = tokio::spawn(async move {
        dogma::set_override(&test_db, module_id, BONUS, 1000.0).await.unwrap();
        dogma::set_override(&test_db, module_id, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_SCAN_RESOLUTION, 1000000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_CAPACITY, 0.0).await.unwrap();
        let duration = effect.attribute(registry::EffectAttribute::Duration).expect("salvage duration");
        dogma::set_override(&test_db, module_id, duration, 2000.0).await.unwrap();
        dogma::set_override(&test_db, module_id, effect.attribute(registry::EffectAttribute::Discharge).unwrap_or(ATTR_CAPACITOR_NEED), 0.0).await.unwrap();
        let target = creation::positioned(&test_db, wreck, f.character, SYSTEM, [50.0, 0.0, 0.0]).await.unwrap();
        scenario_target.store(target, Ordering::Relaxed);
        creation::fitted_items(&test_db, target, f.character, &[(loot_type, FLAG_HI_SLOT_FIRST, 2)]).await.unwrap();
        let loot = loading::load_items(&test_db, ItemQuery::Location(target)).await.unwrap().pop().unwrap();
        assert!(dogma::set_override(&test_db, loot.item_id, BONUS, 17.0).await.unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        test_world.add_ship(SYSTEM, fixture::ball(target, wreck, f.character, None, 50.0)).await.unwrap();
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: SYSTEM };
        targeting_charges::add_target_ctx(&cx, target, true).await.ok().expect("target lock starts");
        tokio::time::timeout(Duration::from_secs(30), async {
            while !targeting_charges::targets_of(f.character).contains(&target) { rx.recv().await.expect("target lock notification"); }
        }).await.expect("target lock completes");
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, generation.clone());
        }
        let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("load fit");
        dogma::set_override(&test_db, module_id, BONUS, -1000000.0).await.unwrap();
        fixture::publish_fit(&test_db, &test_world, SYSTEM, f.character, f.ship).await;
        let (name, args) = rx.try_recv().expect("bonus publication frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(args.len() == 1 && args[0].items().is_some_and(|rows| rows.len() == 1 && rows[0].items()
            .is_some_and(|fields| fields.get(3).and_then(Value::as_int) == Some(i64::from(BONUS)))), "bonus publication row");
        let mut miss = cycle(&cx, module_id, module, target, effect.clone(), generation.clone());
        miss = fixture::capture_cycle(miss).await;
        assert!(matches!(salvage::end(&cx, &mut miss).await.ok().unwrap(), CycleOutcome::Continue));
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        let mut repeated = cycle(&cx, module_id, module, target, effect.clone(), generation.clone());
        repeated.repeat = 2;
        repeated = fixture::capture_cycle(repeated).await;
        fixture::clear_cycles(module_id);
        test_world.with(SYSTEM, move |s| {
            let now = s.now_ms();
            crate::services::effects::lifecycle::cycle_start(s, repeated, now);
        }).await.unwrap();
        tokio::time::timeout(Duration::from_secs(90), async {
            while fixture::completed_cycles(module_id) < 2 { assert!(test_world.after(SYSTEM, 100).await); }
        }).await.expect("two continuing salvage cycles");
        assert_eq!(fixture::completed_cycles(module_id), 2);
        let (name, args) = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("two-cycle salvage retirement frame").expect("pilot notification channel");
        assert_eq!(name, "OnGodmaShipEffect");
        assert_eq!((args.get(3), args.get(4)), (Some(&Value::Int(0)), Some(&Value::Int(0))));
        assert!(loading::load_items(&test_db, ItemQuery::Id(target)).await.unwrap().len() == 1);
        let generation = Arc::new(AtomicBool::new(false));
        combat::state().active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
        combat::state().activation_generation.insert(module_id, generation.clone());
        let mut rejected = cycle(&cx, module_id, module, target, effect.clone(), generation.clone());
        rejected = fixture::capture_cycle(rejected).await;
        fixture::set_cycle_online(&rejected, f.account, false).await;
        let (name, _) = rx.try_recv().expect("offline transition effect frame");
        assert_eq!(name, "OnGodmaShipEffect");
        let (name, _) = rx.try_recv().expect("offline transition attribute frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        let (name, args) = rx.try_recv().expect("offline fitted load frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(args.len() == 1 && args[0].items().is_some_and(|rows| rows.iter().map(|row| {
            let fields = row.items().unwrap_or_default();
            (fields.get(3).and_then(Value::as_int), fields.get(5).and_then(Value::as_float), fields.get(6).and_then(Value::as_float))
        }).eq([(Some(i64::from(ATTR_POWER_LOAD)), Some(0.0), Some(power)),
            (Some(i64::from(ATTR_CPU_LOAD)), Some(0.0), Some(cpu))])), "offline fitted load rows");
        assert!(rx.try_recv().is_err(), "unexpected offline publication frame");
        let rows_before = loading::load_items(&test_db, ItemQuery::Location(target)).await.unwrap();
        assert!(matches!(salvage::start(&cx, &mut rejected).await.unwrap(), CycleOutcome::Stop(_)));
        assert!(matches!(salvage::end(&cx, &mut rejected).await.unwrap(), CycleOutcome::Stop(_)));
        let row_tuple = |row: &loading::GameItem| (row.item_id, row.type_id, row.owner_id,
            row.location_id, row.flag_id, row.quantity, row.singleton, row.custom_info.clone());
        assert_eq!(loading::load_items(&test_db, ItemQuery::Location(target)).await.unwrap()
            .iter().map(row_tuple).collect::<Vec<_>>(), rows_before.iter().map(row_tuple).collect::<Vec<_>>());
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        let received = rx.try_recv().map(|(name, _)| name);
        assert!(received.is_err(), "unexpected event {received:?}");
        fixture::set_cycle_online(&rejected, f.account, true).await;
        let (name, _) = rx.try_recv().expect("online transition effect frame");
        assert_eq!(name, "OnGodmaShipEffect");
        let (name, _) = rx.try_recv().expect("online transition attribute frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        let (name, args) = rx.try_recv().expect("online fitted load frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(args.len() == 1 && args[0].items().is_some_and(|rows| rows.iter().map(|row| {
            let fields = row.items().unwrap_or_default();
            (fields.get(3).and_then(Value::as_int), fields.get(5).and_then(Value::as_float), fields.get(6).and_then(Value::as_float))
        }).eq([(Some(i64::from(ATTR_POWER_LOAD)), Some(power), Some(0.0)),
            (Some(i64::from(ATTR_CPU_LOAD)), Some(cpu), Some(0.0))])), "online fitted load rows");
        assert!(rx.try_recv().is_err(), "unexpected online publication frame");
        fixture::clear_cycles(module_id);
        let generation = Arc::new(AtomicBool::new(false));
        combat::state().active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
        combat::state().activation_generation.insert(module_id, generation.clone());
        dogma::set_override(&test_db, module_id, BONUS, 1000.0).await.unwrap();
        fixture::publish_fit(&test_db, &test_world, SYSTEM, f.character, f.ship).await;
        let (name, args) = rx.try_recv().expect("bonus publication frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(args.len() == 1 && args[0].items().is_some_and(|rows| rows.len() == 1 && rows[0].items()
            .is_some_and(|fields| fields.get(3).and_then(Value::as_int) == Some(i64::from(BONUS)))), "bonus publication row");
        assert!(fit.value(Key::Module(module_id), BONUS) + sde.types.attribute(wreck, DIFFICULTY).unwrap() >= 100.0);
        let mut first = cycle(&cx, module_id, module, target, effect.clone(), generation.clone());
        first = fixture::capture_cycle(first).await;
        assert!(matches!(salvage::start(&cx, &mut first).await.ok().unwrap(), CycleOutcome::Continue));
        test_world.with(SYSTEM, |_| ()).await.unwrap();
        let full_before = crate::dao::tests::item_state(&test_db, &[f.ship, module_id, target, loot.item_id]).await.unwrap();
        assert!(matches!(salvage::end(&cx, &mut first).await.ok().unwrap(), CycleOutcome::Stop("salvage unavailable")));
        assert_eq!(crate::dao::tests::item_state(&test_db, &[f.ship, module_id, target, loot.item_id]).await.unwrap(), full_before);
        assert!(test_world.ball(SYSTEM, target).await.is_some());
        let received = rx.try_recv().map(|(name, _)| name);
        assert!(received.is_err(), "unexpected event {received:?}");
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        if unlimited {
            assert!(dogma::set_cargo_unlimited(&test_db, f.ship, 0.0).await.unwrap());
        } else {
            dogma::set_override(&test_db, f.ship, ATTR_CAPACITY, mode_capacity).await.unwrap();
        }
        fixture::publish_fit(&test_db, &test_world, SYSTEM, f.character, f.ship).await;
        let (name, args) = rx.try_recv().expect("capacity publication attributes");
        assert_eq!(name, "OnModuleAttributeChanges");
        let expected = if unlimited { crate::services::fit::cargo::UNLIMITED_CARGO_WIRE_CAPACITY } else { mode_capacity };
        assert!(args.first().and_then(|list| list.items()).is_some_and(|rows| rows.iter().any(|row| {
            let fields = row.items().unwrap_or_default();
            fields.get(1) == Some(&Value::Int(f.character)) && fields.get(2) == Some(&Value::Int(f.ship))
                && fields.get(3).and_then(Value::as_int) == Some(i64::from(ATTR_CAPACITY))
                && fields.get(5).and_then(Value::as_float) == Some(expected)
                && fields.get(6).and_then(Value::as_float) == Some(0.0)
        })), "capacity publication row");
        let (name, _) = rx.try_recv().expect("capacity publication brain");
        assert_eq!(name, crate::services::dogma::brain::UPDATED_EVENT);
        assert!(rx.try_recv().is_err(), "unexpected extra publication frame");
        first = fixture::capture_cycle(first).await;
        test_db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&loot.item_id, &i32::MAX]).await.unwrap();
        let rollback_before = crate::dao::tests::item_state(&test_db, &[f.ship, module_id, target, loot.item_id]).await.unwrap();
        assert!(salvage::end(&cx, &mut first).await.is_err());
        assert_eq!(crate::dao::tests::item_state(&test_db, &[f.ship, module_id, target, loot.item_id]).await.unwrap(), rollback_before);
        assert!(loading::load_items(&test_db, ItemQuery::Id(target)).await.unwrap().len() == 1);
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        assert!(loading::load_items(&test_db, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await.unwrap().is_empty());
        let received = rx.try_recv().map(|(name, _)| name);
        assert!(received.is_err(), "unexpected event {received:?}");
        test_db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&loot.item_id, &loot_type]).await.unwrap();
        let mut second = cycle(&cx, module_id, module, target, effect, generation);
        second = fixture::capture_cycle(second).await;
        let (a, b) = tokio::join!(salvage::end(&cx, &mut first), salvage::end(&cx, &mut second));
        let outcomes = [a.ok().unwrap(), b.ok().unwrap()];
        assert_eq!(outcomes.iter().filter(|o| matches!(o, CycleOutcome::Ended { reason: "salvage complete", outcome: crate::lua::events::ModuleCycleOutcome::Completed })).count(), 1);
        let events = crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await;
        assert!(matches!(events.as_slice(), [crate::lua::events::GameEvent::WreckSalvaged { char_id, wreck_id, wreck_type, context, .. }]
            if *char_id == f.character && *wreck_id == target && *wreck_type == wreck
                && context.ship_id == Some(f.ship) && context.ship_type_id == Some(hull)
                && context.system_id == Some(SYSTEM) && context.origin == crate::lua::events::resources::FactOrigin::Natural));
        let journal = test_db.query_one("SELECT source_namespace, source_key FROM game.progress_events WHERE kind=$1 AND subject_character_id=$2", &[&"WreckSalvaged", &f.character]).await.unwrap();
        assert_eq!(journal.get::<_, String>("source_namespace"), "business");
        assert_eq!(journal.get::<_, String>("source_key"), format!("WreckSalvaged:{target}"));
        let _ = salvage::end(&cx, &mut first).await;
        assert_eq!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.len(), 1);
        assert!(loading::load_items(&test_db, ItemQuery::Id(target)).await.unwrap().is_empty());
        assert!(test_world.ball(SYSTEM, target).await.is_none());
        let cargo = loading::load_items(&test_db, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await.unwrap();
        assert_eq!(cargo.len(), 1);
        assert!((1..=3).contains(&cargo[0].quantity));
        let material = sde.types.get(cargo[0].type_id).unwrap();
        assert_eq!(material.group_id, Some(MATERIALS));
        assert_eq!(material.published, Some(true));
        // Under Unlimited the yield lands even with a zero finite base; the
        // persisted mode and a real positive volume prove the quota was waived.
        if unlimited {
            let row = dogma::overrides(&test_db, f.ship).await.unwrap().into_iter().find(|r| r.attribute_id == ATTR_CAPACITY).unwrap();
            assert!(row.cargo_unlimited && row.value == 0.0);
            assert!(material.volume.is_some_and(|v| v * cargo[0].quantity as f64 > 0.0));
        }
        let moved = loading::load_items(&test_db, ItemQuery::Id(loot.item_id)).await.unwrap().pop().unwrap();
        assert_eq!((moved.type_id, moved.owner_id, moved.flag_id, moved.quantity, moved.singleton), (loot.type_id, loot.owner_id, loot.flag_id, loot.quantity, loot.singleton));
        let container = loading::load_items(&test_db, ItemQuery::Id(moved.location_id)).await.unwrap().pop().unwrap();
        assert_eq!(sde.types.get(container.type_id).unwrap().group_id, Some(GROUP_CARGO_CONTAINER));
        assert_eq!(container.location_id, SYSTEM);
        let attributes = dogma::overrides(&test_db, loot.item_id).await.unwrap();
        assert!(attributes.iter().any(|a| a.attribute_id == BONUS && a.value == 17.0));
        let container_ball = test_world.ball(SYSTEM, container.item_id).await.unwrap();
        assert_eq!(container_ball.pos, [50.0, 0.0, 0.0]);
    }).await;
    targeting_charges::clear_locks_on(&world, SYSTEM, f.ship, f.character);
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    let owned = loading::load_items(&db, ItemQuery::Owner(f.character)).await.unwrap();
    for item in owned { world.remove_ball(SYSTEM, item.item_id).await; f.items.push(item.item_id); }
    let target = target_id.load(Ordering::Relaxed);
    if target != 0 { world.remove_ball(SYSTEM, target).await; }
    combat::forget_ship(f.ship);
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id, target]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    result.unwrap();
    }
}
