//! Native body and fitted-member replay identities share one allocator.
use super::fixture;
use crate::lua::events::{GameEvent, resources::FactOrigin};
use crate::services::combat::{self, ActiveModule, Ctx};
use crate::services::drones::tests::{fixture::Fixture, resources::delegation, weapon_admission};
use crate::services::effects::{lifecycle::{ActivationOwner, Cycle, Module, NativeCombatGeneration}, registry, space_source::SourceGuard};
use crate::space::combat::RepairSource;
use crate::space::drone::DroneOrder;
use infinity_core::consts::{CATEGORY_MODULE, FLAG_HI_SLOT_FIRST, dogma::{ATTR_DRONE_BANDWIDTH, ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION, EFFECT_REMOTE_ARMOR_REPAIRER}};
use std::sync::{Arc, atomic::AtomicBool};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run this exact test in its own process"]
async fn body_and_modules_keep_independent_native_activations() {
    let mut f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let (other, other_ship) = f.add_pilot().await;
        let (_, target) = f.add_pilot().await;
        let drone_type = weapon_admission::drone_type(&f).unwrap();
        f.train_drone(drone_type).await;
        let lower = crate::space::targeting::next_generation();
        let drone = f.drone(drone_type, 1, true).await;
        f.set(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        f.set(f.ship, ATTR_MAX_TARGET_RANGE, 100_000.0).await;
        fixture::publish_fit(&f.db, &f.world, f.system, f.owner, f.ship).await;
        f.set(drone, ATTR_SCAN_RESOLUTION, 1_000_000.0).await;
        let owner = f.owner;
        delegation::lock(&mut f, owner, target).await.unwrap();
        weapon_admission::launch(&mut f, drone).await.unwrap();
        delegation::command(&mut f, "CmdEngage", &[marshal::Value::list([marshal::Value::Int(drone)]), marshal::Value::Int(target)]).await.unwrap();
        let guard = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Some(Some(guard)) = f.world.with(f.system, move |s| {
                    let d = s.drone(drone)?;
                    if d.order != DroneOrder::Engage(target) { return None; }
                    let guard = SourceGuard::Drone { source: drone, target, generation: d.generation };
                    guard.valid(s).then_some(guard)
                }).await { break guard; }
                assert!(f.world.after(f.system, 100).await);
            }
        }).await.unwrap();
        let effect = registry::resolve(&f.sde, EFFECT_REMOTE_ARMOR_REPAIRER).unwrap();
        let module_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)
            && t.effects().iter().any(|e| e.effect_id == EFFECT_REMOTE_ARMOR_REPAIRER)).unwrap().type_id;
        let mut cycles = Vec::new();
        for (owner, ship) in [(f.owner, f.ship), (other, other_ship)] {
            let module = fixture::item(&f.db, module_type, owner, ship, FLAG_HI_SLOT_FIRST, 1, true, true).await.unwrap();
            fixture::publish_fit(&f.db, &f.world, f.system, owner, ship).await;
            let stop = Arc::new(AtomicBool::new(false));
            combat::state().active.insert(module, ActiveModule { char_id: owner, stop: stop.clone(), forced_inactive: false });
            combat::state().activation_generation.insert(module, stop.clone());
            let generation = crate::space::targeting::next_generation();
            let cx = Ctx { db: f.db.clone(), world: f.world.clone(), notify: f.client.notify_tx.clone(), char_id: owner, ship_id: ship, system_id: f.system };
            let cycle = Cycle { cx, modules: vec![Module { id: module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }],
                presented_effect: effect.id, effect: effect.clone(), target: Some(target), repeat: 1, stop: stop.clone(),
                owner: ActivationOwner { master: module, generation: stop, armed: true,
                    combat_generations: vec![NativeCombatGeneration { module_id: module, generation, sequence: 0 }],
                    pending_combat: None, world: Arc::downgrade(&f.world), system: f.system },
                started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0,
                other: None, repair_amount: 0.0, illegal: false, source_membership: None, target_type: None, inputs: Default::default() };
            cycles.push(fixture::capture_cycle(cycle).await);
        }
        let (body, upper) = f.world.with(f.system, move |s| {
            let body = s.prepare_body_combat_source(drone, None, None, None, FactOrigin::Natural, |s| guard.valid(s)).unwrap();
            let upper = crate::space::targeting::next_generation();
            (body, upper)
        }).await.unwrap();
        let body_generation = u64::from_be_bytes(body.activation[8..].try_into().unwrap());
        assert!(lower < body_generation && body_generation < upper);
        let mut modules = Vec::new();
        for cycle in &mut cycles {
            let module = cycle.modules[0].id;
            modules.push(crate::services::effects::lifecycle::combat_source::prepare_combat_source(cycle, module, Some(module_type)).await.unwrap().unwrap());
        }
        let first = modules[0].facts();
        let second = modules[1].facts();
        assert_ne!(body.activation, first.activation);
        assert_ne!(body.activation, second.activation);
        assert_ne!(first.activation, second.activation);
        assert!(body.sequence > 0);
        assert_eq!((first.sequence, second.sequence), (1, 1));
        assert_eq!((body.actual_actor_character, first.actual_actor_character, second.actual_actor_character), (Some(f.owner), Some(f.owner), Some(other)));
        let facts = [body, first, second];
        let generations = [u64::from_be_bytes(first.activation[8..].try_into().unwrap()), u64::from_be_bytes(second.activation[8..].try_into().unwrap())];
        let ship = f.ship;
        let owner = f.owner;
        let module = cycles[0].modules[0].id;
        let stop = cycles[0].stop.clone();
        let system = f.system;
        let frame = f.world.with(system, move |s| {
            assert!(s.set_condition_fraction_mask(target, 0b010, 0.5));
            assert!(matches!(s.prepare_combat_source(ship, Some(module), first.effect_id, first.weapon_type_id,
                FactOrigin::Natural, generations[0], first.sequence, 1, |s| {
                    !stop.load(std::sync::atomic::Ordering::Relaxed)
                        && combat::state().active.get(&module).is_some_and(|active|
                            active.char_id == owner && !active.forced_inactive && Arc::ptr_eq(&active.stop, &stop))
                        && s.pilot_ball(owner).is_some_and(|ball| ball.id == ship)
                }), Err(crate::space::combat_facts::Refusal::Replay)));
            let mut armor_damage = s.ship_condition(target).unwrap().1;
            assert!(armor_damage >= 3.0, "target must have room for three full repairs");
            for source in facts {
                assert!(s.repair_condition(target, 1, 1.0, RepairSource::Remote(source)).unwrap());
                let after = s.ship_condition(target).unwrap().1;
                assert!((armor_damage - after - 1.0).abs() < 1e-9);
                armor_damage = after;
            }
            assert!(matches!(s.repair_condition(target, 1, 1.0, RepairSource::Remote(body)), Err(crate::space::combat_facts::Refusal::Replay)));
            crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()
        }).await.unwrap();
        for attempt in &mut modules { attempt.disarm(); }
        let events: Vec<_> = frame.events().map(|event| event.event).collect();
        for source in facts {
            assert!(events.iter().any(|event| matches!(event, GameEvent::RemoteRepairApplied {
                source: captured, target: victim, layer: crate::lua::events::combat::HpLayer::Armor, slices, ..
            } if captured.item_id == source.entity.item_id && captured.character_id == source.entity.character_id
                && victim.item_id == target && slices.iter().any(|slice|
                    slice.successful_count == 1 && slice.amount == crate::lua::events::combat::BinaryAmount::from_f64(1.0).unwrap()))));
        }
        drop(cycles.remove(0));
        f.world.with(system, move |s| {
            let next = s.prepare_body_combat_source(drone, None, None, None, FactOrigin::Natural, |s| guard.valid(s)).unwrap();
            assert_eq!(next.activation, body.activation);
            assert!(next.sequence > body.sequence);
            s.finish_combat_attempt(&next);
        }).await.unwrap();
        assert!(f.world.with(system, move |s| s.remove_ball(drone).is_some()).await.unwrap());
        let remaining_module = cycles[0].modules[0].id;
        let mut remaining = crate::services::effects::lifecycle::combat_source::prepare_combat_source(
            &mut cycles[0], remaining_module, Some(module_type)).await.unwrap().unwrap();
        let remaining_facts = remaining.facts();
        assert_eq!((remaining_facts.activation, remaining_facts.sequence), (second.activation, second.sequence + 1));
        f.world.with(system, move |s| s.finish_combat_attempt(&remaining_facts)).await.unwrap();
        remaining.disarm();
        drop(cycles);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
