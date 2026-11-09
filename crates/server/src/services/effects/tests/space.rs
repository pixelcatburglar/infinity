//! Database-backed cloak, micro jump and compression actor regressions.
use super::fixture;
use infinity_core::{consts::{dogma::ATTR_MAX_RANGE, dogma::ATTR_WARP_SCRAMBLE_STATUS, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_CLOAKING, dogma::EFFECT_MICRO_JUMP_DRIVE, dogma::EFFECT_INDUSTRIAL_ITEM_COMPRESSION, FLAG_HI_SLOT_FIRST}};
use crate::{dao::items::dogma, services::{combat::{self, ActiveModule, Ctx, targeting_charges}, effects::{cloak, compression, mjd, EffectKind, lifecycle::{ActivationOwner, Cycle, Module}, registry}, fit::Fit}, space::World};
use marshal::Value;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

async fn scenario(effect_id: i32, kind: u8) {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).expect("ship type").type_id;
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == effect_id)).expect("effect module").type_id;
    let effect = registry::resolve(&sde, effect_id).expect("effect metadata");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, module_type, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        // Overrides are instance dogma values, not type defaults.
        for (item, attr, value) in match kind {
            0 => vec![(f.ship, 1163, 1.0), (f.ship, 2454, 0.0), (f.module, 669, 4000.0), (f.module, 560, 1000.0), (f.ship, ATTR_MAX_TARGET_RANGE, 100000.0), (f.ship, ATTR_SCAN_RESOLUTION, 1000000.0)],
            1 => vec![(f.module, 2066, 100_000.0), (f.ship, ATTR_WARP_SCRAMBLE_STATUS, 0.0)],
            _ => vec![(f.module, 3255, 7.0), (f.module, ATTR_MAX_RANGE, 20000.0)],
        } { dogma::set_override(&test_db, item, attr, value).await.unwrap(); }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: f.module, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("load fit");
        match kind {
            0 => {
                test_world.add_ship(1, fixture::ball(-f.ship, hull, f.character, None, 5000.0)).await.unwrap();
                targeting_charges::add_target_ctx(&cx, -f.ship, true).await.ok().expect("lock target");
                assert!(test_world.after(1, 1200).await);
                assert!(targeting_charges::targets_of(f.character).contains(&-f.ship));
                cloak::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.expect("cloak valid");
                let hp = combat::load_docked_condition(&test_db, &test_world, f.character, f.ship).await.ok().unwrap();
                cloak::start(&cx, &fit, &mut cycle).await.ok().unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    while let Some((name, args)) = rx.recv().await {
                        if name == "OnTarget" && args.first().and_then(Value::as_bytes) == Some(b"lost".as_slice())
                            && args.get(1).and_then(Value::as_int) == Some(-f.ship) { return; }
                    }
                    panic!("target notification channel closed");
                }).await.expect("cloak target loss notification");
                assert!(!targeting_charges::targets_of(f.character).contains(&-f.ship));
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().cloak, 1);
                assert!(cloak::before_lock(&cx).await.is_err());
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().cloak, 0);
                assert!(cloak::before_lock(&cx).await.is_ok());
                assert!(cloak::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.is_err());
                assert_eq!(combat::load_docked_condition(&test_db, &test_world, f.character, f.ship).await.ok().unwrap(), hp);
                assert!(test_world.after(1, 3100).await);
                assert!(cloak::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.is_ok());
                generation.store(false, Ordering::Relaxed);
                cloak::start(&cx, &fit, &mut cycle).await.ok().unwrap();
                assert!(test_world.after(1, 1200).await);
                cloak::before_activate(&cx, f.module, EffectKind::Turret).await.ok().unwrap();
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().cloak, 0);
                let stop_cx = cx.clone();
                test_world.with(1, move |s| cloak::stop(s, &stop_cx, &cycle)).await.unwrap();
            }
            1 => {
                dogma::set_override(&test_db, f.ship, ATTR_WARP_SCRAMBLE_STATUS, 1.0).await.unwrap();
                let blocked = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().unwrap();
                assert!(mjd::validate(&cx, &blocked, &cycle.modules[0], &cycle.effect, None).await.is_err());
                mjd::end(&cx, &blocked, &mut cycle).await.ok().unwrap();
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().pos, [0.0; 3]);
                dogma::set_override(&test_db, f.ship, ATTR_WARP_SCRAMBLE_STATUS, 0.0).await.unwrap();
                mjd::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.ok().unwrap();
                mjd::start(&cx, &fit, &mut cycle).await.ok().unwrap();
                mjd::end(&cx, &fit, &mut cycle).await.ok().unwrap();
                // Normal completion retires the activation before the destiny action executes.
                drop(cycle);
                assert!(test_world.after(1, 1200).await);
                assert_eq!(test_world.ball(1, f.ship).await.unwrap().pos, [0.0, 0.0, -100_000.0]);
            }
            _ => {
                compression::validate(&cx, &fit, &cycle.modules[0], &cycle.effect, None).await.ok().unwrap();
                compression::start(&cx, &fit, &mut cycle).await.ok().unwrap();
                let marker = |b: &crate::space::Ball| b.slim.iter().find(|(key, _)| *key == "compression_facility_typelists").map(|(_, value)| value.clone());
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20000.0))])));
                let other = Arc::new(AtomicBool::new(false));
                let other_generation = other.clone();
                test_world.with(1, move |s| s.set_compressor(f.ship, -f.module, other_generation, 8, 30000.0)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20000.0)), (Value::Int(8), Value::Float(30000.0))])));
                let stale = Arc::new(AtomicBool::new(false));
                let stale_cycle = Cycle { cx: cx.clone(), modules: cycle.modules.clone(), effect: cycle.effect.clone(), target: None, repeat: 1,
                    stop: stale.clone(), owner: ActivationOwner { master: f.module, generation: stale, armed: false, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 }, started: 0,
                    charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
                let stale_cx = cx.clone();
                test_world.with(1, move |s| compression::stop(s, &stale_cx, &stale_cycle)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(7), Value::Float(20000.0)), (Value::Int(8), Value::Float(30000.0))])));
                let stop_cx = cx.clone();
                test_world.with(1, move |s| compression::stop(s, &stop_cx, &cycle)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), Some(Value::Dict(vec![(Value::Int(8), Value::Float(30000.0))])));
                test_world.with(1, move |s| s.remove_compressor(f.ship, -f.module, &other)).await.unwrap();
                assert_eq!(marker(&test_world.ball(1, f.ship).await.unwrap()), None);
            }
        }
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    world.remove_ball(1, -f.ship).await;
    world.remove_ball(1, f.ship).await;
    combat::forget_ship(f.ship);
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn cloak_fitted_delays_and_visibility() { scenario(EFFECT_CLOAKING, 0).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn micro_jump_survives_normal_retirement() { scenario(EFFECT_MICRO_JUMP_DRIVE, 1).await; }

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn compression_advertises_fitted_mapping() { scenario(EFFECT_INDUSTRIAL_ITEM_COMPRESSION, 2).await; }
