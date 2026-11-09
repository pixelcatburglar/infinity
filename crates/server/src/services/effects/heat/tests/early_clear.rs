//! A committed clear between cycle begin and source connection uses the generation
//! recorded with the unready start. The marker goes away, no source connects, and
//! an unrelated module on the same rack keeps its marker and waiter.

use std::sync::{Arc, atomic::AtomicBool};

use infinity_core::consts::{CATEGORY_MODULE, dogma::EFFECT_MICROWARPDRIVE, FLAG_HI_SLOT_FIRST};
use crate::services::combat::{ActiveModule, Ctx};
use crate::services::effects::lifecycle::{ActivationOwner, Cycle, Module};
use crate::services::effects::registry;
use crate::space::{Ball, System, World, MODE_STOP};

use super::super::{boundary_runtime, begin_cycle, clear_committed, state, tick, Entry, Source};

const SHIP: i64 = -910_001;
const OWNER: i64 = -910_002;
const SYSTEM_ID: i64 = -910_003;
const CLEARED: i64 = -910_011;
const SURVIVOR: i64 = -910_022;
const START: i64 = 1_000;
const NOW: i64 = START + 2_000;
const SURVIVOR_AT: i64 = START + 500;

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        let mut active = crate::services::combat::state();
        active.active.remove(&CLEARED);
        active.activation_generation.remove(&CLEARED);
        drop(active);
        let mut heat = state();
        heat.modules.remove(&CLEARED);
        heat.modules.remove(&SURVIVOR);
    }
}

fn ball() -> Ball {
    Ball {
        id: SHIP, type_id: 1, owner_id: OWNER, corp_id: 1, alliance_id: -1,
        char_id: Some(OWNER), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3],
        goto: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: false, massive: true,
        warp: Default::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}


fn cycle(cx: &Ctx, effect: &registry::Effect, module_type: i32, generation: Arc<AtomicBool>, nominal: i64) -> Cycle {
    Cycle {
        cx: cx.clone(),
        modules: vec![Module { id: CLEARED, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }],
        effect: effect.clone(),
        target: None, repeat: 1, stop: generation.clone(),
        owner: ActivationOwner { master: CLEARED, generation, armed: false, combat_generations: Vec::new(),
            pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1_000.0, nominal_start: nominal,
        other: None, repair_amount: 0.0, illegal: false,
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn committed_clear_after_begin_before_connect_releases_only_that_marker() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, EFFECT_MICROWARPDRIVE).expect("microwarpdrive effect");
    let module_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|row| row.effect_id == EFFECT_MICROWARPDRIVE))
        .expect("microwarpdrive module").type_id;
    let world = World::new(db.clone(), sde, 0);
    let _cleanup = Cleanup;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let registry = Arc::new(crate::space::Pilots::default());
    registry.register_pilot(OWNER, tx.clone());
    let mut system = System::new(SYSTEM_ID, registry, NOW);
    system.insert(ball());
    let cx = Ctx { db, world, notify: tx, char_id: OWNER, ship_id: SHIP, system_id: SYSTEM_ID };
    let cleared = Arc::new(AtomicBool::new(false));
    let survivor_generation = Arc::new(AtomicBool::new(false));
    state().modules.insert(CLEARED, Entry {
        owner: OWNER, ship: SHIP, effect: 7, effective: true, desired: true, cx: cx.clone(), source_generation: None,
    });
    state().modules.insert(SURVIVOR, Entry {
        owner: OWNER, ship: SHIP, effect: 8, effective: true, desired: true, cx: cx.clone(),
        source_generation: Some(survivor_generation.clone()),
    });
    {
        let mut active = crate::services::combat::state();
        active.active.insert(CLEARED, ActiveModule { char_id: OWNER, stop: cleared.clone(), forced_inactive: false });
        active.activation_generation.insert(CLEARED, cleared.clone());
    }
    begin_cycle(&mut system, &cycle(&cx, &effect, module_type, cleared.clone(), START));
    assert!(state().modules.get(&CLEARED).and_then(|entry| entry.source_generation.as_ref()).is_some_and(|generation| Arc::ptr_eq(generation, &cleared)));
    let rack = &system.heat.ships[&SHIP].racks[2];
    assert_eq!(boundary_runtime::owned_modules(rack, &[CLEARED], &cleared, START), vec![CLEARED]);
    let stale = Arc::new(AtomicBool::new(false));
    begin_cycle(&mut system, &cycle(&cx, &effect, module_type, stale.clone(), START));
    assert!(state().modules.get(&CLEARED).and_then(|entry| entry.source_generation.as_ref()).is_some_and(|generation| Arc::ptr_eq(generation, &cleared)));
    let rack = &mut system.heat.ships.get_mut(&SHIP).expect("begin installs the rack").racks[2];
    assert!(boundary_runtime::owned_modules(rack, &[CLEARED], &stale, START).is_empty());
    rack.sources.insert(SURVIVOR, Source { flag: FLAG_HI_SLOT_FIRST, rate: 0.0, damage: 0.0,
        suspended: false, stopping: false, generation: survivor_generation.clone() });
    boundary_runtime::install_start(rack, SURVIVOR, survivor_generation.clone(), SURVIVOR_AT);
    let mut receiver = boundary_runtime::register_waiter(rack, SURVIVOR, survivor_generation.clone(), SURVIVOR_AT, vec![SURVIVOR], false);
    let mut cleared_receiver = boundary_runtime::register_waiter(rack, CLEARED, cleared.clone(), START, vec![CLEARED], false);
    tick(&mut system);
    assert_eq!(system.heat.ships[&SHIP].racks[2].last, START);
    assert!(!system.heat.ships[&SHIP].racks[2].sources.contains_key(&CLEARED));
    assert!(receiver.try_recv().is_err());
    let entry = state().modules.remove(&CLEARED).expect("committed membership");
    clear_committed(&mut system, SHIP, CLEARED, entry);
    let rack = &system.heat.ships[&SHIP].racks[2];
    assert_eq!(rack.last, SURVIVOR_AT);
    assert!(!rack.sources.contains_key(&CLEARED));
    assert!(rack.sources.contains_key(&SURVIVOR));
    assert!(boundary_runtime::owned_modules(rack, &[CLEARED], &cleared, START).is_empty());
    assert_eq!(boundary_runtime::owned_modules(rack, &[SURVIVOR], &survivor_generation, SURVIVOR_AT), vec![SURVIVOR]);
    assert!(matches!(cleared_receiver.try_recv(), Ok(None)), "cleared waiter is cancelled");
    assert!(receiver.try_recv().is_err(), "unrelated waiter stays pending");
    assert!(state().modules.contains_key(&SURVIVOR));
    assert!(!state().modules.contains_key(&CLEARED));
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(events.iter().all(|(name, _)| name != "OnHeatAdded" && name != "OnHeatRemoved"));
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn fitting_detach_defers_actor_cleanup_and_preserves_replacement_membership() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde, 0);
    let _cleanup = Cleanup;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let registry = Arc::new(crate::space::Pilots::default());
    registry.register_pilot(OWNER, tx.clone());
    let mut system = System::new(SYSTEM_ID, registry, NOW);
    system.insert(ball());
    let cx = Ctx { db, world, notify: tx, char_id: OWNER, ship_id: SHIP, system_id: SYSTEM_ID };
    let entry = Entry { owner: OWNER, ship: SHIP, effect: 7, effective: true, desired: true,
        cx, source_generation: None };
    state().modules.insert(CLEARED, entry.clone());
    let cleanup = super::super::detach_for_fitting(SHIP, SYSTEM_ID, &[CLEARED]);
    assert!(!state().modules.contains_key(&CLEARED));
    assert!(rx.try_recv().is_err());
    state().modules.insert(CLEARED, entry);
    super::super::apply_fitting_cleanup(&mut system, cleanup);
    assert!(state().modules.get(&CLEARED).is_some_and(|current| current.effective));
    assert!(rx.try_recv().is_ok());
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL; run serially"]
async fn fitting_cleanup_publishes_effective_overload_and_preserves_replacement() {
    use infinity_core::consts::dogma::ATTR_ARMOR_HP;
    use crate::services::combat::groups_health::{Condition, ShipStats};
    use crate::services::fit::{attributes::tests::module_fit, Key};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::types::tests::overload_fixture();
    let world = World::new(db.clone(), Arc::clone(&sde), 0);
    for replacement in [false, true] {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let registry = Arc::new(crate::space::Pilots::default());
        registry.register_pilot(OWNER, tx.clone());
        let mut system = System::new(SYSTEM_ID, registry, NOW);
        system.add_persistent_ship(ball());
        let generation = system.persistent_generation(SHIP).unwrap();
        let before = module_fit(Arc::clone(&sde), 1, 2, true);
        let patch = module_fit(Arc::clone(&sde), 1, 2, true).into_committed_fitting(&before);
        let mut current = module_fit(Arc::clone(&sde), 1, 2, true);
        current.set_module_overloaded(2, true);
        assert_eq!(current.value(Key::Ship, ATTR_ARMOR_HP), 150.0);
        let stats = ShipStats::from_fit(&current, Key::Ship);
        system.install_condition(SHIP, Condition { now: stats.max, stats, shield_at: crate::client::filetime_now() }, current);
        let cx = Ctx { db: db.clone(), world: world.clone(), notify: tx, char_id: OWNER, ship_id: SHIP, system_id: SYSTEM_ID };
        let entry = Entry { owner: OWNER, ship: SHIP, effect: 7, effective: true, desired: true,
            cx, source_generation: None };
        state().modules.insert(2, entry.clone());
        let cleanup = super::super::detach_for_fitting(SHIP, SYSTEM_ID, &[2]);
        if replacement { state().modules.insert(2, entry); }
        assert!(system.publish_fitting(SHIP, OWNER, generation, patch, vec![cleanup]));
        let expected = if replacement { 150.0 } else { 100.0 };
        assert_eq!(system.ship_condition(SHIP).unwrap().1, 0.0);
        if !replacement {
            let mut found = false;
            while let Ok((name, args)) = rx.try_recv() {
                if name != "OnModuleAttributeChanges" { continue; }
                let [marshal::Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
                found = rows.iter().any(|row| {
                    let marshal::Value::Tuple(fields) = row else { return false };
                    fields[3].as_int() == Some(i64::from(ATTR_ARMOR_HP))
                        && fields[5] == marshal::Value::Float(expected)
                        && fields[6] == marshal::Value::Float(150.0)
                });
            }
            assert!(found);
        } else {
            assert!(state().modules.get(&2).is_some_and(|entry| entry.effective));
            let before = module_fit(Arc::clone(&sde), 1, 2, true);
            let offline = module_fit(Arc::clone(&sde), 1, 2, false);
            let patch = offline.into_committed_fitting(&before);
            assert!(system.publish_fitting(SHIP, OWNER, generation, patch, vec![]));
            let mut found = false;
            while let Ok((name, args)) = rx.try_recv() {
                if name != "OnModuleAttributeChanges" { continue; }
                let [marshal::Value::List(rows)] = args.as_slice() else { panic!("expected batch") };
                found |= rows.iter().any(|row| {
                    let marshal::Value::Tuple(fields) = row else { return false };
                    fields[3].as_int() == Some(i64::from(ATTR_ARMOR_HP))
                        && fields[5] == marshal::Value::Float(100.0)
                        && fields[6] == marshal::Value::Float(150.0)
                });
            }
            assert!(found);
        }
        state().modules.remove(&2);
    }
}
