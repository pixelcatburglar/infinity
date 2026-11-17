use super::*;
use infinity_core::{
    consts::{CATEGORY_ENTITY, dogma::{ATTR_AOE_CLOUD_SIZE, EFFECT_ENTITY_CHAIN_LIGHTNING, EFFECT_NPC_BEHAVIOR_SMARTBOMB}},
};
use crate::{
    services::{
        combat::groups_health::{npc_health_fraction, ShipStats},
        effects::{
            parameters::{capture_source, SourceInputs, VortonInputs},
            registry::{resolve, Effect},
            space_source::{self, PreparedCombatAttempt, SourceContext, SourceGuard},
            vorton,
        },
    },
    space::{drone::DronePhase, NpcAction, NpcBrain, NpcConfig, World, HOSTILE_TO_ALL},
};

const NPC_AREA_IDS: [i64; 4] = [9_000_003_200_100, 9_000_003_200_101, 9_000_003_200_102, 9_000_003_200_103];

async fn sde_db() -> (Db, Arc<crate::sde::Sde>) {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    (db, sde)
}

/// An entity type that survives one fitted area hit: finite health stats and a
/// shield reservoir larger than the typed raw damage after resonance.
fn surviving_npc_target(sde: &Arc<crate::sde::Sde>, raw: [f64; 4]) -> i32 {
    sde.types.iter().find(|ty| {
        if ty.category_id != Some(CATEGORY_ENTITY) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        let shield_damage: f64 = raw.into_iter().zip(stats.resonance[0]).map(|(d, r)| d * r).sum();
        stats.max.iter().all(|v| v.is_finite() && *v > 0.0)
            && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
            && stats.shield_tau_ms.is_finite() && stats.shield_tau_ms > 60_000.0
            && shield_damage.is_finite() && shield_damage > 0.0 && shield_damage < stats.max[0] * 0.5
    }).expect("NPC target survives fitted area damage").type_id
}

/// Resolve the SDE entityChainLightning effect and a hull that fits it with
/// real damage, a positive explosion cloud and an arc range, then capture the
/// NPC source packet the way `services/combat` does at `Key::Ship`.
fn npc_vorton_source(sde: &Arc<crate::sde::Sde>) -> (i32, Effect, VortonInputs) {
    let effect = resolve(sde, EFFECT_ENTITY_CHAIN_LIGHTNING).expect("SDE entity chain lightning effect");
    let source_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == EFFECT_ENTITY_CHAIN_LIGHTNING)
        && {
            let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
            fit.value(Key::Ship, ATTR_AOE_CLOUD_SIZE) > 0.0
                && fit.value(Key::Ship, vorton::ATTR_VORTON_ARC_RANGE) > 0.0
                && ATTR_DAMAGE.into_iter().any(|attr| fit.value(Key::Ship, attr) > 0.0)
        }).expect("NPC vorton source with fitted damage, cloud and arc range").type_id;
    let source_fit = Fit::for_space_type(sde.clone(), source_type, vec![]);
    let SourceInputs::Vorton(inputs) = capture_source(&source_fit, Key::Ship, &effect, 0.0).expect("vorton source capture")
        else { panic!("vorton packet") };
    (source_type, effect, inputs)
}

/// Four-ball NPC fixture at the given offsets: a brained source holding a live
/// lock on the primary plus three plain NPC balls. The manually constructed
/// brain has no registered effects; its shot exposes the actual process-unique
/// target generation without starting an automatic effect.
async fn npc_area_fixture(
    db: &Db, sde: &Arc<crate::sde::Sde>, source_type: i32, target_type: i32,
    positions: [f64; 4], signatures: [Option<f64>; 4], aggro_range: f64,
) -> (Arc<World>, i64, u64) {
    let world = World::new(db.clone(), sde.clone(), 1);
    let system = -32001;
    let [source, hostile, nearby, far] = NPC_AREA_IDS;
    world.system(system).await.unwrap();
    let (shots, mut shot_rx) = tokio::sync::mpsc::unbounded_channel();
    world.with(system, move |s| {
        crate::space::snapshot::facts_test_init(s, 6, 4);
        let mut balls = [
            ball(source, positions[0], None), ball(hostile, positions[1], None),
            ball(nearby, positions[2], None), ball(far, positions[3], None)];
        balls[0].type_id = source_type;
        for b in balls.iter_mut().skip(1) { b.type_id = target_type; }
        for (b, signature) in balls.iter_mut().zip(signatures) { b.signature_radius = signature; }
        for b in balls { s.insert(b); }
        let mut brain = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
            fly_range: 1.0, aggro_range, rate_of_fire_ms: 1e9,
            scan_resolution: 17_163.0, repairs: (0.0, 0.0, 1000.0),
        }, Arc::new(move |_, action| {
            if let NpcAction::Shot { generation, .. } = action { let _ = shots.send(generation); }
            Some(Box::pin(async {}))
        }));
        brain.target = Some((hostile, 1, 0)); brain.announced = true;
        assert!(s.add_npc(source, brain));
        for id in [hostile, nearby, far] {
            assert!(s.add_npc(id, NpcBrain::new(None, NpcConfig {
                fly_range: 1.0, aggro_range: 0.0, rate_of_fire_ms: 1e9,
                scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0),
            }, Arc::new(|_, _| Some(Box::pin(async {}))))));
        }
    }).await.unwrap();
    let generation = tokio::time::timeout(std::time::Duration::from_secs(30), shot_rx.recv())
        .await.expect("NPC shot timed out").expect("NPC shot generation");
    (world, system, generation)
}

/// Prepare a fresh admitted NPC attempt inside the actor for one apply; an
/// already-finished attempt is never reused.
async fn npc_attempt(world: &Arc<World>, system: i64, source: i64, effect_id: i32, hostile: i64, generation: u64) -> PreparedCombatAttempt {
    let weak = Arc::downgrade(world);
    world.with(system, move |s| {
        let facts = s.prepare_body_combat_source(source, None, Some(i64::from(effect_id)), None,
            crate::lua::events::resources::FactOrigin::Natural,
            |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
        assert!(facts.module_id.is_none());
        PreparedCombatAttempt::new(facts, weak, system)
    }).await.unwrap()
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_smartbomb_rejects_stale_generation_and_hits_nearby_nonself() {
    let (db, sde) = sde_db().await;
    let effect = resolve(&sde, EFFECT_NPC_BEHAVIOR_SMARTBOMB).unwrap();
    let source_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == EFFECT_NPC_BEHAVIOR_SMARTBOMB)
        && {
            let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
            fit.value(Key::Ship, ATTR_EMP_FIELD_RANGE) > 0.0
                && ATTR_DAMAGE.into_iter().any(|attr| fit.value(Key::Ship, attr) > 0.0)
        }).expect("NPC smartbomb source with fitted damage and radius").type_id;
    let source_fit = Fit::for_space_type(sde.clone(), source_type, vec![]);
    let raw = ATTR_DAMAGE.map(|attr| source_fit.value(Key::Ship, attr).max(0.0));
    let target_type = surviving_npc_target(&sde, raw);
    let radius = source_fit.value(Key::Ship, ATTR_EMP_FIELD_RANGE);
    let (world, system, generation) = npc_area_fixture(&db, &sde, source_type, target_type,
        [0.0, 0.0, 0.0, radius + 1.0], [None, Some(1.0), None, None], 1000.0).await;
    let [source, hostile, nearby, far] = NPC_AREA_IDS;
    let me = world.ball(system, source).await.unwrap();
    let victim = world.ball(system, hostile).await.unwrap();
    let before = world.with(system, move |s| npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap()).await.unwrap();
    assert_eq!(before, 1.0);
    let attempt = npc_attempt(&world, system, source, EFFECT_NPC_BEHAVIOR_SMARTBOMB, hostile, generation).await;
    apply_npc(&db, &world, system, 0, &me, &victim, &effect, 1.0, 0, attempt).await.expect("apply stale NPC smartbomb");
    assert_eq!(world.with(system, move |s| npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap()).await.unwrap(), before);
    let attempt = npc_attempt(&world, system, source, EFFECT_NPC_BEHAVIOR_SMARTBOMB, hostile, generation).await;
    apply_npc(&db, &world, system, 0, &me, &victim, &effect, 1.0, generation, attempt).await.expect("apply NPC smartbomb");
    let (near, distant, own) = world.with(system, move |s| (
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(far).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(source).unwrap(), 0).unwrap(),
    )).await.unwrap();
    assert!(near < before);
    assert_eq!(distant, 1.0);
    assert_eq!(own, 1.0);
    world.with(system, move |s| s.command(far, |ball, _, _| {
        ball.pos = [0.0; 3];
        ball.old_pos = ball.pos;
        Vec::new()
    })).await.unwrap();
    assert!(world.after(system, 1_100).await);
    let before_full = world.with(system, move |s| (
        npc_health_fraction(s.ball(hostile).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(far).unwrap(), 0).unwrap(),
    )).await.unwrap();
    let attempt = npc_attempt(&world, system, source, EFFECT_NPC_BEHAVIOR_SMARTBOMB, hostile, generation).await;
    assert!(matches!(apply_npc(&db, &world, system, 0, &me, &victim, &effect, 1.0, generation, attempt).await,
        Err(CallError::User { msg, .. }) if msg == messages::MSG_CUSTOM_NOTIFY));
    let after_full = world.with(system, move |s| (
        npc_health_fraction(s.ball(hostile).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(far).unwrap(), 0).unwrap(),
    )).await.unwrap();
    assert_eq!(after_full, before_full);
    let frame = world.with(system, |s| crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()).await.unwrap();
    let mut targets = Vec::new();
    for captured in frame.events() {
        if let crate::lua::events::GameEvent::DamageApplied { char_id, source: attacker, target, module_id, .. } = captured.event {
            assert!(char_id.is_none());
            assert!(module_id.is_none());
            assert_eq!(attacker.item_id, source);
            targets.push(target.item_id);
        }
    }
    targets.sort_unstable();
    targets.dedup();
    assert_eq!(targets, vec![hostile, nearby]);
    world.with(system, move |s| { for id in NPC_AREA_IDS { s.remove_ball(id); } }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_vorton_uses_hull_values_and_broad_area_candidates() {
    let (db, sde) = sde_db().await;
    let (source_type, effect, inputs) = npc_vorton_source(&sde);
    let target_type = surviving_npc_target(&sde, inputs.raw);
    let arc = inputs.arc_range;
    let (world, system, generation) = npc_area_fixture(&db, &sde, source_type, target_type,
        [0.0, arc * 0.5, arc * 0.5 + 10.0, arc * 2.0],
        [None, Some(inputs.aoe_cloud), Some(inputs.aoe_cloud * 0.5), None], arc * 4.0).await;
    let [source, hostile, nearby, far] = NPC_AREA_IDS;
    let me = world.ball(system, source).await.unwrap();
    let victim = world.ball(system, hostile).await.unwrap();
    let attempt = npc_attempt(&world, system, source, EFFECT_ENTITY_CHAIN_LIGHTNING, hostile, generation).await;
    let packet = SourceInputs::Vorton(inputs);
    let cx = SourceContext {
        db: &db, world: &world, system, source: &me, target: &victim,
        module: 0, effect: &effect, duration_ms: 1.0, inputs: &packet,
        guard: SourceGuard::Npc { source, target: hostile, generation },
        attacker: None, drone: None, phase: DronePhase::Begin,
    };
    space_source::apply(&cx, Some(attempt)).await.expect("apply NPC vorton");
    let (primary, near, distant, own) = world.with(system, move |s| (
        npc_health_fraction(s.ball(hostile).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(far).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(source).unwrap(), 0).unwrap(),
    )).await.unwrap();
    // The primary is only present because it is prepended explicitly; its larger
    // signature applies more than the nearby NPC secondary, which is eligible
    // under the same broad area policy that needs no hostility check.
    assert!(primary < near);
    assert!(near < 1.0);
    assert_eq!(distant, 1.0);
    assert_eq!(own, 1.0);
    world.with(system, move |s| { for id in NPC_AREA_IDS { s.remove_ball(id); } }).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn area_vorton_releases_stale_source_and_target_attempts() {
    let (db, sde) = sde_db().await;
    let (source_type, _, inputs) = npc_vorton_source(&sde);
    let target_type = surviving_npc_target(&sde, inputs.raw);
    let arc = inputs.arc_range;
    let (world, system, generation) = npc_area_fixture(&db, &sde, source_type, target_type,
        [0.0, arc * 0.5, arc * 0.5 + 10.0, arc * 2.0],
        [None, Some(inputs.aoe_cloud), Some(inputs.aoe_cloud), None], arc * 4.0).await;
    let [source, hostile, nearby, _far] = NPC_AREA_IDS;
    let victims_of = move |world: &Arc<World>| {
        let world = world.clone();
        async move {
            world.with(system, move |s| vec![
                (hostile, s.ball(hostile).unwrap().char_id, s.combat_membership_generation(hostile), false, None),
                (nearby, s.ball(nearby).unwrap().char_id, s.combat_membership_generation(nearby), false, None),
            ]).await
        }
    };
    // A stale source generation fails the shared guard: no damage, and the
    // admitted attempt is finished instead of applying or leaking.
    let attempt = npc_attempt(&world, system, source, EFFECT_ENTITY_CHAIN_LIGHTNING, hostile, generation).await;
    apply_damage_guarded(&db, &world, system, source_type, victims_of(&world).await.unwrap(),
        DamageApplication::Vorton(inputs), Some(DamageGuard::Npc(source, hostile, 0)), attempt)
        .await.expect("stale source vorton apply");
    let stale = world.with(system, move |s| (
        npc_health_fraction(s.ball(hostile).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap())).await.unwrap();
    assert_eq!(stale, (1.0, 1.0));
    // A stale target membership skips only that victim; the primary still applies.
    let mut victims = victims_of(&world).await.unwrap();
    victims[1].2 = victims[1].2.map(|generation| generation.wrapping_add(1));
    let attempt = npc_attempt(&world, system, source, EFFECT_ENTITY_CHAIN_LIGHTNING, hostile, generation).await;
    apply_damage_guarded(&db, &world, system, source_type, victims,
        DamageApplication::Vorton(inputs), Some(DamageGuard::Npc(source, hostile, generation)), attempt)
        .await.expect("stale target vorton apply");
    let (primary, skipped) = world.with(system, move |s| (
        npc_health_fraction(s.ball(hostile).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap())).await.unwrap();
    assert!(primary < 1.0);
    assert_eq!(skipped, 1.0);
    // A fresh attempt with current facts applies to every victim: the earlier
    // attempts were finished and released rather than left pending.
    let attempt = npc_attempt(&world, system, source, EFFECT_ENTITY_CHAIN_LIGHTNING, hostile, generation).await;
    apply_damage_guarded(&db, &world, system, source_type, victims_of(&world).await.unwrap(),
        DamageApplication::Vorton(inputs), Some(DamageGuard::Npc(source, hostile, generation)), attempt)
        .await.expect("fresh vorton apply");
    let (primary, secondary) = world.with(system, move |s| (
        npc_health_fraction(s.ball(hostile).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap())).await.unwrap();
    assert!(primary < 1.0 && secondary < 1.0);
    world.with(system, move |s| { for id in NPC_AREA_IDS { s.remove_ball(id); } }).await.unwrap();
}
