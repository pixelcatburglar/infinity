//! Transactional salvage through the fitted effect, persistent inventory and system actor.
use super::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use infinity_core::{consts::{dogma::ATTR_CAPACITOR_NEED, dogma::ATTR_CAPACITY, dogma::ATTR_MAX_RANGE, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_SALVAGING, FLAG_CARGO, FLAG_HI_SLOT_FIRST, GROUP_CARGO_CONTAINER, GROUP_SALVAGED_MATERIALS, GROUP_WRECK}};
use infinity_core::consts::dogma::{ATTR_ACCESS_DIFFICULTY_BONUS, ATTR_CPU, ATTR_CPU_LOAD, ATTR_POWER, ATTR_POWER_LOAD};
use crate::{dao::items::{creation, dogma, loading::{self, ItemQuery}}, services::{combat::{self, ActiveModule, Ctx, targeting_charges}, effects::{lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module}, registry, salvage}, fit::{Fit, Key}}, space::World};
use marshal::Value;
use std::{future::Future, pin::Pin, sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}}, time::Duration};

// SDE dogma accessDifficulty.
const DIFFICULTY: i32 = 901;
const SYSTEM: i64 = 1;

fn cycle(cx: &Ctx, module: i64, kind: i32, target: i64, effect: registry::Effect, generation: Arc<AtomicBool>) -> Cycle {
    Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: kind, flag: FLAG_HI_SLOT_FIRST }], presented_effect: effect.id, effect,
        target: Some(target), repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module, generation, armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
        source_membership: None, target_type: None, inputs: Default::default() }
}
/// The drone has locked and published the salvaging cycle, so the same actor
/// decision has queued Begin. Admission stays unread until the real finish.
async fn salvage_begun(f: &crate::services::drones::tests::fixture::Fixture, drone: i64, wreck: i64) -> anyhow::Result<crate::space::drone::DroneRecord> {
    use crate::services::drones::tests::shared::wait_until;
    use infinity_core::consts::DRONE_STATE_SALVAGING;
    wait_until(f, "salvage begin", move |s| {
        let record = s.drone(drone)?;
        (record.state == DRONE_STATE_SALVAGING && record.generation != 0 && s.drone_valid(drone, wreck, record.generation)).then(|| record)
    }).await
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
    let loot_type = sde.types.iter().find(|t| t.group_id == Some(GROUP_SALVAGED_MATERIALS) && t.published == Some(true) && t.volume.is_some_and(|v| v > 0.0)).expect("published material").type_id;
    assert!(sde.types.group_types(GROUP_SALVAGED_MATERIALS).iter().filter_map(|id| sde.types.get(*id)).filter(|t| t.published == Some(true)).all(|t| t.volume.is_some_and(|v| v > 0.0)));
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
        dogma::set_override(&test_db, module_id, ATTR_ACCESS_DIFFICULTY_BONUS, 1000.0).await.unwrap();
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
        assert!(dogma::set_override(&test_db, loot.item_id, ATTR_ACCESS_DIFFICULTY_BONUS, 17.0).await.unwrap());
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
        dogma::set_override(&test_db, module_id, ATTR_ACCESS_DIFFICULTY_BONUS, -1000000.0).await.unwrap();
        fixture::publish_fit(&test_db, &test_world, SYSTEM, f.character, f.ship).await;
        let (name, args) = rx.try_recv().expect("bonus publication frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(args.len() == 1 && args[0].items().is_some_and(|rows| rows.len() == 1 && rows[0].items()
            .is_some_and(|fields| fields.get(3).and_then(Value::as_int) == Some(i64::from(ATTR_ACCESS_DIFFICULTY_BONUS)))), "bonus publication row");
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
        dogma::set_override(&test_db, module_id, ATTR_ACCESS_DIFFICULTY_BONUS, 1000.0).await.unwrap();
        fixture::publish_fit(&test_db, &test_world, SYSTEM, f.character, f.ship).await;
        let (name, args) = rx.try_recv().expect("bonus publication frame");
        assert_eq!(name, "OnModuleAttributeChanges");
        assert!(args.len() == 1 && args[0].items().is_some_and(|rows| rows.len() == 1 && rows[0].items()
            .is_some_and(|fields| fields.get(3).and_then(Value::as_int) == Some(i64::from(ATTR_ACCESS_DIFFICULTY_BONUS)))), "bonus publication row");
        assert!(fit.value(Key::Module(module_id), ATTR_ACCESS_DIFFICULTY_BONUS) + sde.types.attribute(wreck, DIFFICULTY).unwrap() >= 100.0);
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
        assert_eq!(material.group_id, Some(GROUP_SALVAGED_MATERIALS));
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
        assert!(attributes.iter().any(|a| a.attribute_id == ATTR_ACCESS_DIFFICULTY_BONUS && a.value == 17.0));
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

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn two_admitted_drone_recoveries_claim_one_wreck() {
    crate::tests::init_config();
    let mut f = crate::services::drones::tests::fixture::Fixture::new().await;
    let result: Result<(), anyhow::Error> = async {
        use anyhow::{anyhow, ensure, Context};
        use crate::space::drone::DroneOrder;
        use crate::services::effects::salvage::recovery;
        use crate::services::drones::tests::shared::{launch_salvager, salvage_types, wait_until, wreck};
        let (ty, duration, range, wreck_ty, _) = salvage_types(&f.sde);
        f.train_drone(ty).await;
        let wreck = wreck(&f, wreck_ty, f.owner, 50.0).await?;
        // A long cycle keeps the natural end out of the race.
        let d1 = launch_salvager(&mut f, ty, duration, range, 0.0, 300_000.0).await?;
        let d2 = launch_salvager(&mut f, ty, duration, range, 0.0, 300_000.0).await?;
        let (owner, ship, system, sde) = (f.owner, f.ship, f.system, f.sde.clone());
        let mut admitted = Vec::new();
        for d in [d1, d2] {
            ensure!(f.world.with(system, move |s| s.drone_order(d, owner, ship, DroneOrder::Salvage(Some(wreck)))).await.context("system stopped")?, "salvage order refused");
            let generation = f.world.with(system, move |s| s.drone(d).map(|d| d.generation)).await.context("system stopped")?.context("drone missing")?;
            let token = wait_until(&f, "salvage admission", move |s| s.drone_salvage_admit(d, generation, wreck)).await?;
            admitted.push((d, generation, token));
        }
        let wreck_ball = f.world.ball(system, wreck).await.context("wreck ball gone")?;
        let mut admitted = admitted.into_iter();
        let (i1, g1, (m1, c1, _)) = admitted.next().context("first admission")?;
        let (i2, g2, (m2, c2, _)) = admitted.next().context("second admission")?;
        let request = |item, generation, membership, cancelled| recovery::RecoveryRequest {
            system, character: owner, ship, wreck: wreck_ball.clone(), range: 500.0,
            authority: recovery::RecoveryAuthority::Drone { item, generation, membership, cancelled },
            destination: recovery::RewardDestination::Drone { item, owner, type_id: ty },
            yields: recovery::select_yields(&sde).expect("yield draw"),
        };
        let (first, second) = tokio::join!(
            recovery::settle(&f.db, &f.world, request(i1, g1, m1, c1), None),
            recovery::settle(&f.db, &f.world, request(i2, g2, m2, c2), None));
        let mut won = 0;
        for (d, g, outcome) in [(i1, g1, first), (i2, g2, second)] {
            match outcome.map_err(|e| anyhow!("settle: {e:?}"))? {
                recovery::RecoveryOutcome::Recovered => { won += 1; let _ = f.world.with(system, move |s| s.drone_salvage_carried(d, g)).await; }
                recovery::RecoveryOutcome::Unavailable => { let _ = f.world.with(system, move |s| s.drone_salvage_resume(d, g)).await; }
                recovery::RecoveryOutcome::Deferred { unknown, error } =>
                    return Err(anyhow!("deferred outcome is not a recovery result: unknown={unknown} {error:?}")),
            }
        }
        ensure!(won == 1, "two admitted ends did not race to exactly one claim");
        ensure!(crate::db::outbox::tests::events(&f.db, "WreckSalvaged", owner).await.len() == 1, "claim journaled twice");
        let aboard = loading::load_items(&f.db, ItemQuery::Location(i1)).await?.into_iter()
            .chain(loading::load_items(&f.db, ItemQuery::Location(i2)).await?).collect::<Vec<_>>();
        ensure!(aboard.len() == 1 && sde.types.get(aboard[0].type_id).and_then(|t| t.group_id) == Some(GROUP_SALVAGED_MATERIALS), "exactly one drone must carry the yield");
        Ok(())
    }.await;
    f.cleanup().await;
    result.expect("shared drone recovery claim");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn transient_wreck_deferred_and_drone_settlement_are_real() {
    crate::tests::init_config();
    let pool_name = format!("npc_salv_p_{}", crate::handshake::random_u64());
    let mut config = crate::config::tests::test_database();
    config.application_name = pool_name.clone();
    let mut f = crate::services::drones::tests::fixture::Fixture::with_database_config(config).await;
    let module_holder = Arc::new(AtomicI64::new(0));
    let scenario_module = module_holder.clone();
    let control = crate::services::inventory::npc_loot::tests::support::control_client().await;
    let tag = format!("npc_salv_gate_{}", f.owner);
    let business_name = format!("{}_b", pool_name);
    let commit_db = crate::services::inventory::npc_loot::tests::support::named_db(&business_name).await;
    commit_db.query_one("SELECT 1", &[]).await.unwrap();
    control.observe_pools(&[pool_name.clone(), business_name.clone()]).await;
    let mut roots = Vec::new();
    let mut result: Option<Result<(), anyhow::Error>> = None;
    type Proof = Pin<Box<dyn Future<Output = Result<salvage::recovery::RecoveryOutcome, crate::services::CallError>>>>;
    async fn poll_proof(proof: &mut Proof, poisoned: &mut bool, completed: &mut bool) -> Result<salvage::recovery::RecoveryOutcome, crate::services::CallError> {
        std::future::poll_fn(|cx| {
            *poisoned = true;
            let result = proof.as_mut().poll(cx);
            *poisoned = false;
            if result.is_ready() { *completed = true; }
            result
        }).await
    }
    let mut contradiction: Option<Proof> = None;
    let mut restored: Option<Proof> = None;
    let (mut contradiction_poisoned, mut contradiction_done) = (false, false);
    let (mut restored_poisoned, mut restored_done) = (false, false);
    let scenario = crate::tests::catch_scenario(async {
    result = Some(async {
        use anyhow::{anyhow, ensure, Context};
        use crate::space::drone::DroneOrder;
        use crate::services::effects::salvage::recovery;
        use crate::services::drones::tests::shared::{launch_salvager, salvage_types, transient_wreck, wait_until};
        use crate::services::inventory::npc_loot::tests::support::drive_gated;
        let (ty, duration, range, wreck_ty, material) = salvage_types(&f.sde);
        f.train_drone(ty).await;
        let module_ty = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| e.effect_id == EFFECT_SALVAGING)).context("salvager")?.type_id;
        let effect = registry::resolve(&f.sde, EFFECT_SALVAGING).unwrap();
        creation::fitted_items(&f.db, f.ship, f.owner, &[(module_ty, FLAG_HI_SLOT_FIRST, 1)]).await?;
        let module_id = loading::load_items(&f.db, ItemQuery::LocationFlags(f.ship, &[FLAG_HI_SLOT_FIRST])).await?.pop()
            .context("fitted module")?.item_id;
        scenario_module.store(module_id, Ordering::Relaxed);
        let (owner, ship, system, sde) = (f.owner, f.ship, f.system, f.sde.clone());
        // The module authority is real: installed combat fit gives membership,
        // the active state binds the generation Arc, the lock holds the ball.
        dogma::set_override(&f.db, module_id, ATTR_ACCESS_DIFFICULTY_BONUS, 1000.0).await?;
        dogma::set_override(&f.db, module_id, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0).await?;
        let fit = Fit::load(&f.db, sde.clone(), owner, ship).await.map_err(|e| anyhow!("fit: {e:?}"))?;
        let capacity = crate::services::inventory::hold_storage::HoldCapacity::capture(&fit, FLAG_CARGO).map_err(|e| anyhow!("capacity: {e:?}"))?;
        let seed = combat::prepare_condition_with_fit(&f.db, ship, &fit).await.map_err(|e| anyhow!("condition: {e:?}"))?;
        let membership = f.world.with(system, move |s| {
            s.install_condition(ship, seed.into_condition(), fit);
            s.combat_membership_generation(ship)
        }).await.context("system stopped")?.context("combat membership")?;
        let generation = Arc::new(AtomicBool::new(false));
        combat::state().active.insert(module_id, ActiveModule { char_id: owner, stop: generation.clone(), forced_inactive: false });
        combat::state().activation_generation.insert(module_id, generation.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        f.world.register_pilot(owner, tx.clone());
        let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: tx, char_id: owner, ship_id: ship, system_id: system };
        let module_auth = |generation: Arc<AtomicBool>| recovery::RecoveryAuthority::Module {
            master: module_id, generation, membership, presented_effect: effect.id };
        let request = |wreck: crate::space::Ball, authority, destination| recovery::RecoveryRequest {
            system, character: owner, ship, wreck, range: 500.0,
            authority, destination, yields: recovery::select_yields(&sde).expect("yield draw") };
        let view_world = f.world.clone();
        let view_ids = |root: i64| view_world.with(system, move |s| match s.npc_wreck_recovery_view(
            &crate::space::system::npc_wrecks::NpcWreckAccess::GmCleanup { character: 1 }, root) {
            crate::space::system::npc_wrecks::NpcWreckLookup::Ready(v) => Some(v.items.iter().map(|i| (i.item_id, i.quantity)).collect::<Vec<_>>()),
            _ => None });
        let debris_ty = sde.types.iter().filter(|t| t.group_id == Some(GROUP_CARGO_CONTAINER))
            .min_by_key(|t| t.type_id).context("container type")?.type_id;
        // A: a held foreign claim defers both adapters without retrying a draw.
        let root = transient_wreck(&f, wreck_ty, owner, 50.0, vec![(material, 4)]).await?;
        roots.push(root);
        let loot_ids = view_ids(root).await.context("loot view")?;
        targeting_charges::add_target_ctx(&cx, root, true).await.map_err(|e| anyhow!("target lock: {e:?}"))?;
        tokio::time::timeout(Duration::from_secs(30), async {
            while !targeting_charges::targets_of(owner).contains(&root) { rx.recv().await.context("lock frame")?; }
            Ok::<(), anyhow::Error>(())
        }).await??;
        let org = crate::dao::characters::members::organization(&f.db, owner).await?;
        let ship_type = f.world.ball(system, ship).await.context("ship ball")?.type_id;
        let event = crate::lua::events::GameEvent::WreckSalvaged {
            char_id: owner, wreck_id: root, wreck_type: wreck_ty, occurred_at: crate::client::filetime_now(),
            context: crate::lua::events::resources::FactContext {
                corporation_id: org.as_ref().map(|o| o.corporation_id),
                alliance_id: org.and_then(|o| o.alliance_id), faction_id: None,
                ship_id: Some(ship), ship_type_id: Some(ship_type), system_id: Some(system),
                station_id: None, origin: crate::lua::events::resources::FactOrigin::Natural },
        };
        let yields = recovery::select_yields(&sde).map_err(|e| anyhow!("yield draw: {e:?}"))?;
        let held_generation = generation.clone();
        let held = f.world.with(system, move |s| {
            let access = crate::space::system::npc_wrecks::NpcWreckAccess::Recovery {
                character: owner, ship, source: ship, source_token: s.capture_ball(ship).expect("ship token"), range: 500.0,
                attempt: crate::space::system::npc_wrecks::RecoveryAttempt::Module { master: module_id, stop: held_generation } };
            let crate::space::system::npc_wrecks::NpcWreckLookup::Ready(view) = s.npc_wreck_recovery_view(&access, root) else { panic!("view") };
            match s.claim_npc_wreck(&view.identity, access,
                crate::space::system::npc_wrecks::NpcWreckOperation::Salvage { character: owner, destination: ship, yields, event }) {
                crate::space::system::npc_wrecks::NpcWreckLookup::Ready(guard) => guard, _ => panic!("claim") }
        }).await.context("system stopped")?;
        let d = launch_salvager(&mut f, ty, duration, range, 0.0, 300_000.0).await?;
        ensure!(f.world.with(system, move |s| s.drone_order(d, owner, ship, DroneOrder::Salvage(Some(root)))).await.context("stopped")?, "salvage order refused");
        let rec = salvage_begun(&f, d, root).await?;
        let wreck_ball = f.world.ball(system, root).await.context("wreck ball")?;
        ensure!(crate::services::drones::salvage::finish_cycle(&f.db, &f.world, system, rec.clone(), wreck_ball.clone(), 500.0, 1000.0).await.is_ok(),
            "the busy root maps to the blocked order hold");
        ensure!(f.world.with(system, move |s| s.drone_salvage_admit(d, rec.generation, root).is_none()).await.context("stopped")?,
            "a blocked order does not re-admit on the same generation");
        let out = recovery::settle(&f.db, &f.world, request(wreck_ball.clone(), module_auth(generation.clone()),
            recovery::RewardDestination::Ship { ship, capacity }), None).await.map_err(|e| anyhow!("settle: {e:?}"))?;
        ensure!(matches!(out, recovery::RecoveryOutcome::Deferred { unknown: false, .. }), "a live held claim must defer the module");
        let mut module_cycle = fixture::capture_cycle(cycle(&cx, module_id, module_ty, root, effect.clone(), generation.clone())).await;
        ensure!(salvage::end(&cx, &mut module_cycle).await.is_err(), "module adapter must report live Deferred");
        // Release the foreign guard, then the real drone finishes the round:
        // it admits, settles, carries, and the replacement can keeps the ids.
        ensure!(held.finish(crate::space::system::npc_wrecks::NpcWreckDisposition::RolledBack).await);
        ensure!(view_ids(root).await == Some(loot_ids.clone()), "a deferred attempt moved nothing");
        ensure!(f.world.with(system, move |s| s.drone_order(d, owner, ship, DroneOrder::Salvage(Some(root)))).await.context("stopped")?, "re-order refused");
        let rec = salvage_begun(&f, d, root).await?;
        crate::services::drones::salvage::finish_cycle(&f.db, &f.world, system, rec, wreck_ball, 500.0, 1000.0).await.map_err(|e| anyhow!("drone settle: {e:?}"))?;
        let carried = loading::load_items(&f.db, ItemQuery::Location(d)).await?;
        ensure!(carried.len() == 1 && sde.types.get(carried[0].type_id).and_then(|t| t.group_id) == Some(GROUP_SALVAGED_MATERIALS),
            "the drone destination holds the yield");
        let cargo = loading::load_items(&f.db, ItemQuery::LocationFlags(ship, &[FLAG_CARGO])).await?;
        ensure!(cargo.iter().all(|i| sde.types.get(i.type_id).and_then(|t| t.group_id) != Some(GROUP_SALVAGED_MATERIALS)),
            "no yield landed in the ship hold this round");
        ensure!(crate::db::outbox::tests::events(&f.db, "WreckSalvaged", owner).await.len() == 1, "claim journaled once");
        let can = f.world.with(system, move |s| s.balls().find(|b| b.type_id == debris_ty).map(|b| b.id))
            .await.context("stopped")?.context("replacement can")?;
        ensure!(view_ids(can).await == Some(loot_ids.clone()), "original loot ids survive in the memory can");
        ensure!(f.db.query_opt("SELECT 1 FROM game.items WHERE item_id=ANY($1)", &[&loot_ids.as_ref().unwrap().iter().map(|&(id, _)| id).collect::<Vec<_>>()]).await?.is_none());
        // B: a COMMIT-cancelled module settle seals Unknown; a contradictory
        // stored receipt defers true; restoring it finishes the delta once.
        let root2 = transient_wreck(&f, wreck_ty, owner, 60.0, vec![(material, 3)]).await?;
        roots.push(root2);
        let loot2 = view_ids(root2).await.context("loot view")?;
        targeting_charges::add_target_ctx(&cx, root2, true).await.map_err(|e| anyhow!("target lock: {e:?}"))?;
        tokio::time::timeout(Duration::from_secs(30), async {
            while !targeting_charges::targets_of(owner).contains(&root2) { rx.recv().await.context("lock frame")?; }
            Ok::<(), anyhow::Error>(())
        }).await??;
        let gate = 0x5abc0003_i32 ^ (owner as i32);
        control.gate_fn(&tag, gate).await;
        control.client.execute(&format!("CREATE CONSTRAINT TRIGGER {tag} AFTER INSERT ON game.progress_events DEFERRABLE INITIALLY DEFERRED
            FOR EACH ROW WHEN (NEW.source_key='WreckSalvaged:{root2}') EXECUTE FUNCTION game.{tag}()"), &[]).await.context("trigger")?;
        control.hold(gate).await;
        let wreck2 = f.world.ball(system, root2).await.context("wreck ball")?;
        let mut business: Pin<Box<dyn Future<Output = Result<recovery::RecoveryOutcome, crate::services::CallError>> + '_>> =
            Box::pin(recovery::settle(&commit_db, &f.world, request(wreck2.clone(), module_auth(generation.clone()),
                recovery::RewardDestination::Ship { ship, capacity }), None));
        let (ready, _) = drive_gated(&f.db, &control, &mut business, "%commit%").await;
        if let Some(result) = ready {
            match result {
                Err(error) => return Err(anyhow!("settle failed before COMMIT gate: {error:?}")),
                Ok(_) => return Err(anyhow!("settle returned an outcome before COMMIT gate")),
            }
        }
        drop(business);
        let sealed = wait_until(&f, "sealed claim", move |s| match s.npc_wreck_recovery_view(
            &crate::space::system::npc_wrecks::NpcWreckAccess::GmCleanup { character: 1 }, root2) {
            crate::space::system::npc_wrecks::NpcWreckLookup::Busy(Some(claim))
                if s.npc_wreck_claim_status(&claim) == Some(crate::space::system::npc_wrecks::NpcWreckClaimState::Unknown) => Some(claim),
            _ => None }).await?;
        control.release().await;
        let key = format!("WreckSalvaged:{root2}");
        // The committed receipt becomes visible asynchronously; wait for the
        // real row, then perturb one field BEFORE any proof runs, without a race.
        let original: String = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Some(row) = f.db.query_opt("SELECT origin FROM game.progress_events WHERE source_key=$1", &[&key]).await.unwrap() {
                    break row.get::<_, String>("origin");
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("committed receipt never became visible");
        f.db.execute("UPDATE game.progress_events SET origin='client' WHERE source_key=$1", &[&key]).await?;
        let proof_request = request(wreck2.clone(), module_auth(generation.clone()), recovery::RewardDestination::Ship { ship, capacity });
        let (proof_db, proof_world) = (f.db.clone(), f.world.clone());
        contradiction = Some(Box::pin(async move { recovery::settle(&proof_db, &proof_world, proof_request, None).await }));
        let out = poll_proof(contradiction.as_mut().expect("contradiction proof"), &mut contradiction_poisoned, &mut contradiction_done)
            .await.map_err(|e| anyhow!("settle: {e:?}"))?;
        ensure!(matches!(out, recovery::RecoveryOutcome::Deferred { unknown: true, .. }), "a contradictory receipt defers true");
        let mut module_cycle = fixture::capture_cycle(cycle(&cx, module_id, module_ty, root2, effect.clone(), generation.clone())).await;
        ensure!(salvage::end(&cx, &mut module_cycle).await.is_err(), "module adapter must report unknown Deferred");
        ensure!(f.world.with(system, { let sealed = sealed.clone(); move |s| s.npc_wreck_claim_status(&sealed) }).await
            == Some(Some(crate::space::system::npc_wrecks::NpcWreckClaimState::Unknown)), "the seal survives the contradiction");
        // A drone that never admitted hits the sealed root through the real
        // adapter: Deferred maps to the unknown hold and an error out.
        let d2 = launch_salvager(&mut f, ty, duration, range, 0.0, 300_000.0).await?;
        ensure!(f.world.with(system, move |s| s.drone_order(d2, owner, ship, DroneOrder::Salvage(Some(root2)))).await.context("stopped")?, "order refused");
        let rec2 = salvage_begun(&f, d2, root2).await?;
        ensure!(crate::services::drones::salvage::finish_cycle(&f.db, &f.world, system, rec2.clone(), wreck2.clone(), 500.0, 1000.0).await.is_err(),
            "the sealed root maps to the unknown hold");
        ensure!(f.world.with(system, move |s| s.drone_salvage_admit(d2, rec2.generation, root2).is_none()).await.context("stopped")?,
            "an unknown-latched order does not re-admit");
        // Restore the exact stored row: the proof applies the frozen delta once.
        f.db.execute("UPDATE game.progress_events SET origin=$2 WHERE source_key=$1", &[&key, &original]).await?;
        let proof_request = request(wreck2, module_auth(generation.clone()), recovery::RewardDestination::Ship { ship, capacity });
        let (proof_db, proof_world) = (f.db.clone(), f.world.clone());
        restored = Some(Box::pin(async move { recovery::settle(&proof_db, &proof_world, proof_request, None).await }));
        let out = poll_proof(restored.as_mut().expect("restored proof"), &mut restored_poisoned, &mut restored_done)
            .await.map_err(|e| anyhow!("settle: {e:?}"))?;
        ensure!(matches!(out, recovery::RecoveryOutcome::Unavailable), "a proven commit leaves this attempt unavailable");
        let can2 = f.world.with(system, move |s| s.balls().filter(|b| b.type_id == debris_ty && b.id != can).map(|b| b.id).collect::<Vec<_>>())
            .await.context("stopped")?.into_iter().max().context("replacement can")?;
        ensure!(view_ids(can2).await == Some(loot2.clone()), "original loot ids survive in the memory can");
        ensure!(crate::db::outbox::tests::events(&f.db, "WreckSalvaged", owner).await.len() == 2, "exactly one journal per wreck");
        Ok(())
    }.await);
    }).await;
    let mut cleanup = Vec::new();
    let release = crate::tests::catch_scenario(async {
        control.client.execute("COMMIT", &[]).await.expect("release test gate during cleanup");
    }).await;
    let released = release.is_ok();
    cleanup.push(release);
    if released {
        for (proof, poisoned, completed) in [
            (&mut contradiction, &mut contradiction_poisoned, &mut contradiction_done),
            (&mut restored, &mut restored_poisoned, &mut restored_done),
        ] {
            if let Some(proof) = proof.as_mut() {
                if !*poisoned && !*completed {
                    cleanup.push(crate::tests::catch_scenario(async {
                        if let Err(error) = tokio::time::timeout(Duration::from_secs(30), poll_proof(proof, poisoned, completed))
                            .await.expect("salvage proof did not finish during cleanup") {
                            panic!("salvage proof returned an error during cleanup: {error:?}");
                        }
                    }).await);
                }
            }
        }
    }
    drop(contradiction);
    drop(restored);
    let mut safe = false;
    if released {
        let pools = crate::tests::catch_scenario(async { control.wait_pools(&[pool_name, business_name]).await; }).await;
        let backends = crate::tests::catch_scenario(async { control.wait_backends().await; }).await;
        safe = pools.is_ok() && backends.is_ok();
        cleanup.extend([pools, backends]);
        if safe {
            cleanup.push(crate::tests::catch_scenario(async {
                control.gate_uninstall(&tag, &["game.progress_events"]).await;
            }).await);
            cleanup.push(crate::tests::catch_scenario(async {
                for root in roots {
                    let key = format!("WreckSalvaged:{root}");
                    f.db.execute("DELETE FROM game.progress_deliveries WHERE event_id IN (SELECT event_id FROM game.progress_events WHERE source_key=$1)", &[&key]).await.unwrap();
                    f.db.execute("DELETE FROM game.progress_events WHERE source_key=$1", &[&key]).await.unwrap();
                }
            }).await);
        }
    }
    drop(commit_db);
    let mut driver = control.into_driver();
    cleanup.push(crate::tests::catch_scenario(async {
        crate::services::inventory::npc_loot::tests::support::wait_driver(&mut driver).await;
    }).await);
    if safe {
    cleanup.push(crate::tests::catch_scenario(async {
        combat::forget_ship(f.ship);
        let module_id = module_holder.load(Ordering::Relaxed);
        if module_id != 0 {
            let mut state = combat::state();
            if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
            state.activation_generation.remove(&module_id);
        }
    }).await);
    cleanup.push(crate::tests::catch_scenario(async { f.cleanup().await; }).await);
    }
    crate::services::inventory::npc_loot::tests::support::report_cleanup_failures(&cleanup);
    if let Err(panic) = scenario {
        std::panic::resume_unwind(panic);
    }
    if let Err(error) = result.expect("scenario returned") {
        panic!("transient deferred/settle coverage: {error:?}");
    }
    for outcome in cleanup { if let Err(panic) = outcome { std::panic::resume_unwind(panic); } }
}

