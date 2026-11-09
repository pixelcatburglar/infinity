use super::*;
use infinity_core::{
    consts::{CATEGORY_ENTITY, dogma::EFFECT_NPC_BEHAVIOR_SMARTBOMB},
};
use crate::{
    services::combat::groups_health::{npc_health_fraction, ShipStats},
    space::{NpcAction, NpcBrain, NpcConfig, World, HOSTILE_TO_ALL},
};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_smartbomb_rejects_stale_generation_and_hits_nearby_nonself() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let effect = super::super::super::registry::resolve(&sde, EFFECT_NPC_BEHAVIOR_SMARTBOMB).unwrap();
    let source_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == EFFECT_NPC_BEHAVIOR_SMARTBOMB)
        && {
            let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
            fit.value(Key::Ship, ATTR_EMP_FIELD_RANGE) > 0.0
                && ATTR_DAMAGE.into_iter().any(|attr| fit.value(Key::Ship, attr) > 0.0)
        }).expect("NPC smartbomb source with fitted damage and radius").type_id;
    let source_fit = Fit::for_space_type(sde.clone(), source_type, vec![]);
    let raw = ATTR_DAMAGE.map(|attr| source_fit.value(Key::Ship, attr).max(0.0));
    let target_type = sde.types.iter().find(|ty| {
        if ty.category_id != Some(CATEGORY_ENTITY) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        let shield_damage: f64 = raw.into_iter().zip(stats.resonance[0]).map(|(d, r)| d * r).sum();
        stats.max.iter().all(|v| v.is_finite() && *v > 0.0)
            && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
            && stats.shield_tau_ms.is_finite() && stats.shield_tau_ms > 60_000.0
            && shield_damage.is_finite() && shield_damage > 0.0 && shield_damage < stats.max[0] * 0.5
    }).expect("NPC target survives fitted smartbomb damage").type_id;
    let radius = source_fit.value(Key::Ship, ATTR_EMP_FIELD_RANGE);
    let world = World::new(db.clone(), sde, 1);
    let system = -32001;
    let source = 9_000_003_200_100;
    let hostile = source + 1;
    let nearby = source + 2;
    let far = source + 3;
    world.system(system).await.unwrap();
    let (shots, mut shot_rx) = tokio::sync::mpsc::unbounded_channel();
    world.with(system, move |s| {
        crate::space::snapshot::facts_test_init(s, 6, 4);
        let mut me = ball(source, 0.0, None); me.type_id = source_type;
        let mut target = ball(hostile, 0.0, None); target.type_id = target_type; target.signature_radius = Some(1.0);
        let mut other = ball(nearby, 0.0, None); other.type_id = target_type;
        let mut outside = ball(far, radius + 1.0, None); outside.type_id = target_type;
        for b in [me, target, other, outside] { s.insert(b); }
        let mut brain = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
            fly_range: 1.0, aggro_range: 1000.0, rate_of_fire_ms: 1e9,
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
    // This manually constructed brain has no registered effects; its shot exposes
    // the actual process-unique generation without starting an automatic smartbomb.
    let generation = tokio::time::timeout(std::time::Duration::from_secs(30), shot_rx.recv())
        .await.expect("NPC shot timed out").expect("NPC shot generation");
    let me = world.ball(system, source).await.unwrap();
    let victim = world.ball(system, hostile).await.unwrap();
    let before = world.with(system, move |s| npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap()).await.unwrap();
    assert_eq!(before, 1.0);
    let weak = Arc::downgrade(&world);
    let attempt = world.with(system, move |s| {
        let facts = s.prepare_body_combat_source(source, None, Some(EFFECT_NPC_BEHAVIOR_SMARTBOMB as i64), None,
            crate::lua::events::resources::FactOrigin::Natural, |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
        super::super::super::space_source::PreparedCombatAttempt::new(facts, weak, system)
    }).await.unwrap();
    apply_npc(&db, &world, system, 0, &me, &victim, &effect, 1.0, 0, attempt).await.expect("apply stale NPC smartbomb");
    assert_eq!(world.with(system, move |s| npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap()).await.unwrap(), before);
    let weak = Arc::downgrade(&world);
    let attempt = world.with(system, move |s| {
        let facts = s.prepare_body_combat_source(source, None, Some(EFFECT_NPC_BEHAVIOR_SMARTBOMB as i64), None,
            crate::lua::events::resources::FactOrigin::Natural, |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
        assert!(facts.module_id.is_none());
        super::super::super::space_source::PreparedCombatAttempt::new(facts, weak, system)
    }).await.unwrap();
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
    let weak = Arc::downgrade(&world);
    let attempt = world.with(system, move |s| {
        let facts = s.prepare_body_combat_source(source, None, Some(EFFECT_NPC_BEHAVIOR_SMARTBOMB as i64), None,
            crate::lua::events::resources::FactOrigin::Natural, |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
        super::super::super::space_source::PreparedCombatAttempt::new(facts, weak, system)
    }).await.unwrap();
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
    world.with(system, move |s| { for id in [source, hostile, nearby, far] { s.remove_ball(id); } }).await.unwrap();
}
