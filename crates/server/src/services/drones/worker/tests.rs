use super::*;

#[test]
fn admission_accepts_supported_drone_families_but_not_salvage() {
    for kind in [EffectKind::Turret, EffectKind::Missile, EffectKind::Mining,
        EffectKind::RemoteRepair { layer: 0 }, EffectKind::RemoteRepair { layer: 1 },
        EffectKind::RemoteRepair { layer: 2 }, EffectKind::Neutralizer, EffectKind::Ecm,
        EffectKind::Projected { jamming: JammingType::Webify },
        EffectKind::Projected { jamming: JammingType::Paint },
        EffectKind::Projected { jamming: JammingType::Damp },
        EffectKind::Projected { jamming: JammingType::TrackingDisrupt }] {
        assert!(supported(kind), "{kind:?}");
    }
    for kind in [EffectKind::Salvage, EffectKind::Smartbomb, EffectKind::Modifiers,
        EffectKind::Projected { jamming: JammingType::Scramble }] {
        assert!(!supported(kind), "{kind:?}");
    }
}

#[tokio::test]
#[ignore = "requires a dedicated database with the retail SDE"]
async fn persisted_drone_types_always_have_a_physical_spec() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut salvage = 0;
    let mut turret = 0;
    for ty in sde.types.iter().filter(|ty| ty.category_id == Some(infinity_core::consts::CATEGORY_DRONE)) {
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let spec = spec(&fit, Key::Ship, ty.type_id).unwrap();
        assert_eq!(spec.type_id, ty.type_id);
        let kinds: Vec<_> = sde.types.effects(ty.type_id).iter()
            .filter_map(|row| registry::resolve(&sde, row.effect_id).map(|effect| effect.kind)).collect();
        if kinds.contains(&EffectKind::Salvage) && !kinds.iter().copied().any(supported) {
            assert!(spec.effect_id.is_none());
            salvage += 1;
        }
        if kinds.contains(&EffectKind::Turret) && spec.effect_id.is_some() { turret += 1; }
    }
    assert!(salvage > 0 && turret > 0);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn phase_rebases_worker_fit_without_replacing_live_condition() {
    use infinity_core::consts::{CATEGORY_DRONE, dogma::*};
    use crate::services::drones::tests::fixture::Fixture;
    use marshal::Value;

    let mut f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let ty = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
            && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
            && t.effects().iter().any(|e| registry::resolve(&f.sde, e.effect_id).is_some_and(|e| e.kind == EffectKind::Turret)))
            .expect("published turret drone").type_id;
        f.train_drone(ty).await;
        let (_, target) = f.add_pilot_with_signature(Some(10_000.0)).await;
        let id = f.drone(ty, 1, true).await;
        for (item, attr, value) in [(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
            (id, ATTR_SHIELD_CAPACITY, 1000.0), (id, ATTR_SHIELD_RECHARGE_RATE, 1e30),
            (id, ATTR_SHIELD_EM_DAMAGE_RESONANCE, 1.0), (id, ATTR_SCAN_RESOLUTION, 1_000_000.0),
            (id, ATTR_MAX_TARGET_RANGE, 1_000_000.0), (id, ATTR_MAX_RANGE, 1_000_000.0),
            (id, ATTR_RATE_OF_FIRE, 60_000.0)] { f.set(item, attr, value).await; }
        let args = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
        assert_eq!(f.call("ship", "LaunchDrones", &args).await.ok().unwrap(), Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]));
        let (owner, ship, system) = (f.owner, f.ship, f.system);
        assert!(f.world.with(system, move |s| s.drone_order(id, owner, ship, DroneOrder::Engage(target))).await.unwrap());
        let drone = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Some(drone) = f.world.with(system, move |s| {
                    let drone = s.drone(id)?;
                    s.drone_valid(id, target, drone.generation).then_some(drone)
                }).await.flatten() { break drone; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("drone did not lock target");
        let generation = drone.generation;
        let facts = f.world.with(system, move |s| s.prepare_body_combat_source(id, None, None, Some(ty),
            crate::lua::events::resources::FactOrigin::Natural,
            move |s| SourceGuard::Drone { source: id, target, generation }.valid(s))).await.unwrap().unwrap();
        assert_eq!(facts.entity.item_id, id);
        assert_eq!(facts.actual_actor_character, Some(owner));
        assert_eq!(facts.context.ship_id, Some(ship));
        assert!(facts.controller_incarnation.is_some());
        assert!(!facts.source_is_npc);
        f.world.with(system, move |s| s.finish_combat_attempt(&facts)).await.unwrap();
        let hit = f.world.with(system, move |s| {
            let facts = s.prepare_body_combat_source(id, None, None, Some(ty),
                crate::lua::events::resources::FactOrigin::Natural,
                move |s| SourceGuard::Drone { source: id, target, generation }.valid(s)).unwrap();
            s.apply_damage(id, [1.0, 0.0, 0.0, 0.0],
                crate::services::combat::damage::DamageSource { facts: Some(facts), ..Default::default() }, false)
        }).await.unwrap().unwrap().unwrap();
        assert!(hit.dealt > 0.0 && !hit.lethal);
        let before = f.world.with(system, move |s| s.combat_condition_snapshot(id).unwrap()).await.unwrap();
        let fit_revision = f.world.with(system, move |s| s.combat_fit_revision(id).unwrap()).await.unwrap();
        cycle(&f.db, &f.world, system, drone.clone(), target, DronePhase::End, None).await.ok().unwrap();
        let (after, next_revision) = f.world.with(system, move |s| (s.combat_condition_snapshot(id).unwrap(), s.combat_fit_revision(id).unwrap())).await.unwrap();
        assert!(next_revision > fit_revision, "phase did not publish its loaded fit");
        assert_eq!(after.condition.now, before.condition.now);
        for cancellation in 0..3 {
            assert!(f.world.with(system, move |s| s.drone_order(id, owner, ship, DroneOrder::Engage(target))).await.unwrap());
            let generation = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                loop {
                    if let Some(generation) = f.world.with(system, move |s| {
                        let d = s.drone(id)?;
                        s.drone_valid(id, target, d.generation).then_some(d.generation)
                    }).await.flatten() { break generation; }
                    assert!(f.world.after(system, 100).await);
                }
            }).await.expect("drone did not relock");
            let weak = Arc::downgrade(&f.world);
            let pending = f.world.with(system, move |s| {
                let facts = s.prepare_body_combat_source(id, None, Some(1), None,
                    crate::lua::events::resources::FactOrigin::Natural,
                    move |s| SourceGuard::Drone { source: id, target, generation }.valid(s)).unwrap();
                s.drone_store_combat_attempt(id, generation, effects::space_source::PreparedCombatAttempt::new(facts, weak, system));
                facts
            }).await.unwrap();
            f.world.with(system, move |s| match cancellation {
                0 => { assert!(s.drone_order(id, owner, ship, DroneOrder::Idle)); }
                1 => s.drone_jammed(id),
                _ => s.drone_controller_lost(ship),
            }).await.unwrap();
            let (result, unchanged) = f.world.with(system, move |s| {
                let before = s.combat_condition_snapshot(id).unwrap().condition.now;
                let result = s.repair_condition(id, 0, 0.1, crate::space::combat::RepairSource::Remote(pending));
                (result, before == s.combat_condition_snapshot(id).unwrap().condition.now)
            }).await.unwrap();
            assert!(matches!(result, Err(crate::space::combat_facts::Refusal::Replay)));
            assert!(unchanged, "a canceled cast must not apply a ghost repair");
        }
        f.world.with(system, move |s| s.drone_controller_lost(ship)).await.unwrap();
        assert!(f.world.with(system, move |s| s.prepare_body_combat_source(id, None, None, Some(ty),
            crate::lua::events::resources::FactOrigin::Natural,
            move |s| SourceGuard::Drone { source: id, target, generation }.valid(s))).await.unwrap().is_err());
        let revision_before_stale_end = f.world.with(system, move |s| s.combat_fit_revision(id)).await.unwrap();
        cycle(&f.db, &f.world, system, drone, target, DronePhase::End, None).await.ok().unwrap();
        assert_eq!(f.world.with(system, move |s| s.combat_fit_revision(id)).await.unwrap(), revision_before_stale_end);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn delayed_worker_repair_retains_actor_admitted_source() {
    use infinity_core::consts::{CATEGORY_DRONE, dogma::*};
    use crate::services::drones::tests::fixture::Fixture;
    use marshal::Value;
    let mut f = Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let ty = f.sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_DRONE)
            && ty.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)
            && ty.effects().iter().filter_map(|row| registry::resolve(&f.sde, row.effect_id))
                .find(|effect| supported(effect.kind))
                .is_some_and(|effect| matches!(effect.kind, EffectKind::RemoteRepair { layer: 1 })))
            .expect("published armor repair drone").type_id;
        f.train_drone(ty).await;
        let id = f.drone(ty, 1, true).await;
        for (item, attr, value) in [(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0),
            (id, ATTR_SCAN_RESOLUTION, 1_000_000.0), (id, ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (id, ATTR_MAX_RANGE, 1_000_000.0), (id, ATTR_ARMOR_DAMAGE_AMOUNT, 1.0)] {
            f.set(item, attr, value).await;
        }
        let effect_attrs: Vec<_> = f.sde.types.effects(ty).iter()
            .filter_map(|row| registry::resolve(&f.sde, row.effect_id))
            .flat_map(|effect| [
                (effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE), 60_000.0),
                (effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE), 1_000_000.0),
            ]).collect();
        for (attr, value) in effect_attrs { f.set(id, attr, value).await; }
        let args = [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)];
        f.call("ship", "LaunchDrones", &args).await.ok().unwrap();
        let (system, owner, ship) = (f.system, f.owner, f.ship);
        assert!(f.world.with(system, move |s| {
            s.set_condition_fraction_mask(ship, 1 << 1, 0.5);
            s.drone_order(id, owner, ship, DroneOrder::Engage(ship))
        }).await.unwrap());
        let drone = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if let Some(drone) = f.world.with(system, move |s| {
                    let drone = s.drone(id)?;
                    s.drone_valid(id, ship, drone.generation).then_some(drone)
                }).await.flatten() { break drone; }
                assert!(f.world.after(system, 100).await);
            }
        }).await.expect("repair drone did not lock");
        let generation = drone.generation;
        let effect_id = drone.spec.effect_id.expect("live drone must retain its repair effect");
        assert!(drone.spec.duration_ms >= 1.0);
        assert!(registry::resolve(&f.sde, effect_id).is_some_and(|effect| effect.kind == EffectKind::RemoteRepair { layer: 1 }));
        let callback = effects(f.db.clone(), Arc::downgrade(&f.world), system);
        let (begin, facts) = f.world.with(system, move |s| {
            let future = callback(s, DroneAction::Cycle { drone, target: ship, phase: DronePhase::Begin }).expect("admitted begin");
            let token = s.drone_take_combat_attempt(id, generation).expect("begin must retain token before worker runs");
            let facts = token.facts();
            s.drone_store_combat_attempt(id, generation, token);
            (future, facts)
        }).await.unwrap();
        assert_eq!(facts.actual_actor_character, Some(owner));
        assert_eq!(facts.context.ship_id, Some(ship));
        let frozen_corp = facts.context.corporation_id;
        f.world.with(system, move |s| s.command(ship, |ball, _, _| { ball.corp_id = 42; Vec::new() })).await.unwrap();
        assert!(f.world.after(system, 1_100).await);
        assert_eq!(facts.context.corporation_id, frozen_corp);
        begin.await;
        let callback = effects(f.db.clone(), Arc::downgrade(&f.world), system);
        let end = f.world.with(system, move |s| {
            let drone = s.drone(id).unwrap();
            callback(s, DroneAction::Cycle { drone, target: ship, phase: DronePhase::End })
        }).await.unwrap().expect("admitted end");
        let before = f.world.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition.now[1]).await.unwrap();
        end.await;
        let after = f.world.with(system, move |s| s.combat_condition_snapshot(ship).unwrap().condition.now[1]).await.unwrap();
        assert!(after > before, "the pre-admitted cast did not repair");
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
