//! Transactional salvage through the fitted effect, persistent inventory and system actor.
use super::fixture;
use infinity_core::{consts::{dogma::ATTR_CAPACITY, dogma::ATTR_MAX_RANGE, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_SALVAGING, FLAG_CARGO, FLAG_HI_SLOT_FIRST, GROUP_CARGO_CONTAINER, GROUP_WRECK}};
use crate::{dao::items::{creation, dogma, loading::{self, ItemQuery}}, services::{combat::{self, ActiveModule, Ctx, targeting_charges}, effects::{lifecycle::{ActivationOwner, Cycle, CycleOutcome, Module}, registry, salvage}, fit::{Fit, Key}}, space::World};
use std::{sync::{Arc, atomic::{AtomicBool, AtomicI64, Ordering}}, time::Duration};

// SDE dogma accessDifficulty and accessDifficultyBonus; invGroups Salvaged Materials.
const DIFFICULTY: i32 = 901;
const BONUS: i32 = 902;
const MATERIALS: i32 = 754;
const SYSTEM: i64 = 1;

fn cycle(cx: &Ctx, module: i64, kind: i32, target: i64, effect: registry::Effect, generation: Arc<AtomicBool>) -> Cycle {
    Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: kind, flag: FLAG_HI_SLOT_FIRST }], effect,
        target: Some(target), repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module, generation, armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn salvage_preserves_loot_refuses_full_cargo_and_claims_once() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.capacity.is_some_and(|v| v > 0.0) && t.fit_attribute(ATTR_CAPACITY).is_some_and(|v| v > 0.0)).expect("cargo ship").type_id;
    let module = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == EFFECT_SALVAGING)).expect("salvager").type_id;
    let wreck = sde.types.iter().find(|t| t.group_id == Some(GROUP_WRECK) && t.attribute(DIFFICULTY).is_some_and(|v| v >= -200.0)).expect("wreck with difficulty").type_id;
    let loot_type = sde.types.iter().find(|t| t.group_id == Some(MATERIALS) && t.published == Some(true) && t.volume.is_some_and(|v| v > 0.0)).expect("published material").type_id;
    assert!(sde.types.group_types(MATERIALS).iter().filter_map(|id| sde.types.get(*id)).filter(|t| t.published == Some(true)).all(|t| t.volume.is_some_and(|v| v > 0.0)));
    assert_eq!(crate::config::game().salvage.salvage_material_stacks, 1);
    assert_eq!(crate::config::game().salvage.salvage_quantity_min, 1);
    assert_eq!(crate::config::game().salvage.salvage_quantity_max, 3);
    let effect = registry::resolve(&sde, EFFECT_SALVAGING).unwrap();
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let target_id = Arc::new(AtomicI64::new(0));
    let scenario_target = target_id.clone();
    let result = tokio::spawn(async move {
        dogma::set_override(&test_db, f.module, BONUS, 1000.0).await.unwrap();
        dogma::set_override(&test_db, f.module, effect.attribute(registry::EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 10000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_SCAN_RESOLUTION, 1000000.0).await.unwrap();
        dogma::set_override(&test_db, f.ship, ATTR_CAPACITY, 0.0).await.unwrap();
        let target = creation::positioned(&test_db, wreck, f.character, SYSTEM, [50.0, 0.0, 0.0]).await.unwrap();
        scenario_target.store(target, Ordering::Relaxed);
        creation::fitted_items(&test_db, target, f.character, &[(loot_type, FLAG_HI_SLOT_FIRST, 2)]).await.unwrap();
        let loot = loading::load_items(&test_db, ItemQuery::Location(target)).await.unwrap().pop().unwrap();
        assert!(dogma::set_override(&test_db, loot.item_id, BONUS, 17.0).await.unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        test_world.add_ship(SYSTEM, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await.unwrap();
        test_world.add_ship(SYSTEM, fixture::ball(target, wreck, f.character, None, 50.0)).await.unwrap();
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: SYSTEM };
        targeting_charges::add_target_ctx(&cx, target, true).await.ok().expect("target lock starts");
        tokio::time::timeout(Duration::from_secs(30), async {
            while !targeting_charges::targets_of(f.character).contains(&target) { rx.recv().await.expect("target lock notification"); }
        }).await.expect("target lock completes");
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, generation.clone());
        }
        let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("load fit");
        dogma::set_override(&test_db, f.module, BONUS, -1000000.0).await.unwrap();
        let miss_fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().unwrap();
        let mut miss = cycle(&cx, f.module, module, target, effect.clone(), generation.clone());
        assert!(matches!(salvage::end(&cx, &miss_fit, &mut miss).await.ok().unwrap(), CycleOutcome::Continue));
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        dogma::set_override(&test_db, f.module, BONUS, 1000.0).await.unwrap();
        assert!(fit.value(Key::Module(f.module), BONUS) + sde.types.attribute(wreck, DIFFICULTY).unwrap() >= 100.0);
        let mut first = cycle(&cx, f.module, module, target, effect.clone(), generation.clone());
        assert!(matches!(salvage::start(&cx, &fit, &mut first).await.ok().unwrap(), CycleOutcome::Continue));
        test_world.with(SYSTEM, |_| ()).await.unwrap();
        while rx.try_recv().is_ok() {}
        let full_before = fixture::fingerprint(&test_db).await.unwrap();
        assert!(matches!(salvage::end(&cx, &fit, &mut first).await.ok().unwrap(), CycleOutcome::Stop("salvage unavailable")));
        assert_eq!(fixture::fingerprint(&test_db).await.unwrap(), full_before);
        assert!(test_world.ball(SYSTEM, target).await.is_some());
        assert!(rx.try_recv().is_err());
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        dogma::set_override(&test_db, f.ship, ATTR_CAPACITY, 1000000.0).await.unwrap();
        let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("reload fit");
        test_db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&loot.item_id, &i32::MAX]).await.unwrap();
        let rollback_before = fixture::fingerprint(&test_db).await.unwrap();
        assert!(salvage::end(&cx, &fit, &mut first).await.is_err());
        assert_eq!(fixture::fingerprint(&test_db).await.unwrap(), rollback_before);
        assert!(loading::load_items(&test_db, ItemQuery::Id(target)).await.unwrap().len() == 1);
        assert!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.is_empty());
        assert!(loading::load_items(&test_db, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await.unwrap().is_empty());
        assert!(rx.try_recv().is_err());
        test_db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&loot.item_id, &loot_type]).await.unwrap();
        let mut second = cycle(&cx, f.module, module, target, effect, generation);
        let (a, b) = tokio::join!(salvage::end(&cx, &fit, &mut first), salvage::end(&cx, &fit, &mut second));
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
        let _ = salvage::end(&cx, &fit, &mut first).await;
        assert_eq!(crate::db::outbox::tests::events(&test_db, "WreckSalvaged", f.character).await.len(), 1);
        assert!(loading::load_items(&test_db, ItemQuery::Id(target)).await.unwrap().is_empty());
        assert!(test_world.ball(SYSTEM, target).await.is_none());
        let cargo = loading::load_items(&test_db, ItemQuery::LocationFlags(f.ship, &[FLAG_CARGO])).await.unwrap();
        assert_eq!(cargo.len(), 1);
        assert!((1..=3).contains(&cargo[0].quantity));
        let material = sde.types.get(cargo[0].type_id).unwrap();
        assert_eq!(material.group_id, Some(MATERIALS));
        assert_eq!(material.published, Some(true));
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
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    let owned = loading::load_items(&db, ItemQuery::Owner(f.character)).await.unwrap();
    for item in owned { world.remove_ball(SYSTEM, item.item_id).await; }
    let target = target_id.load(Ordering::Relaxed);
    if target != 0 { world.remove_ball(SYSTEM, target).await; }
    combat::forget_ship(f.ship);
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
