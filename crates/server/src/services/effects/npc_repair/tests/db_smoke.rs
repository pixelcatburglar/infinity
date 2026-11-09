use super::*;
use std::sync::Arc;
use infinity_core::consts::CATEGORY_ENTITY;
use crate::{
    services::combat::{damage::apply_damage_guarded, groups_health::ShipStats},
    services::fit::{Fit, Key},
    space::{NpcBrain, NpcConfig, WarpState, MODE_STOP},
};

const CYCLE_MS: f64 = 500.0;
const CYCLE_WAIT_MS: u64 = 800;
const MOVE_CYCLE_MS: f64 = 3_000.0;
const MOVE_WAIT_MS: u64 = 3_300;

fn ball(id: i64, type_id: i32, owner: i64, pilot: Option<i64>) -> Ball {
    Ball {
        id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: pilot,
        signature_radius: pilot.map(|_| 1.0), mode: MODE_STOP, radius: 1.0,
        mass: 1e6, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3],
        segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
        free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn brain(hostile: Option<i64>, shots: Option<tokio::sync::mpsc::UnboundedSender<u64>>) -> NpcBrain {
    let mut brain = NpcBrain::new(hostile.map(|_| crate::space::HOSTILE_TO_ALL), NpcConfig {
        fly_range: 1.0, aggro_range: 1000.0, rate_of_fire_ms: 1e9,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 1000.0),
    }, Arc::new(move |_, action| {
        if let crate::space::NpcAction::Shot { generation, .. } = action {
            if let Some(tx) = &shots { let _ = tx.send(generation); }
        }
        Some(Box::pin(async {}))
    }));
    if let Some(id) = hostile {
        brain.target = Some((id, 1, 0));
        brain.announced = true;
    }
    brain
}
async fn run_source(db: &crate::db::Db, world: &Arc<crate::space::World>, system: i64, me: &Ball, victim: &Ball, effect: &Effect, duration_ms: f64, generation: u64) -> Result<(), CallError> {
    let fit = crate::services::fit::Fit::for_space_type(world.sde().clone(), me.type_id, vec![]);
    let source = me.id;
    let target = victim.id;
    let effect_id = effect.id;
    let weak = Arc::downgrade(world);
    let attempt = world.with(system, move |s| {
        let facts = s.prepare_body_combat_source(source, None, Some(effect_id), None,
            crate::lua::events::resources::FactOrigin::Natural,
            move |s| s.npc_projection_valid(source, target, generation)).expect("native NPC admission");
        PreparedCombatAttempt::new(facts, weak, system)
    }).await.expect("NPC system");
    assert_eq!(attempt.facts().entity.item_id, source);
    assert_eq!(attempt.facts().actual_actor_character, None);
    assert!(attempt.facts().source_is_npc);
    assert_eq!(attempt.facts().effect_id, Some(effect_id));
    apply_source(&SourceContext { db, world, system, source: me, target: victim, module: 0, effect, duration_ms, fit: &fit,
        key: crate::services::fit::Key::Ship, guard: SourceGuard::Npc { source: me.id, target: victim.id, generation },
        attacker: None, drone: None, phase: DronePhase::Begin }, Some(attempt)).await
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_repair_real_sde_phase_selection_and_guards() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let target_type = sde.types.iter().find(|ty| {
        if ty.category_id != Some(CATEGORY_ENTITY) { return false; }
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, vec![]);
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        stats.max.iter().all(|v| v.is_finite() && *v > 0.0)
            && stats.resonance.iter().flatten().all(|v| v.is_finite() && *v >= 0.0)
            && stats.resonance.iter().all(|layer| layer[0] > 0.0)
            && stats.shield_tau_ms.is_finite() && stats.shield_tau_ms > 60_000.0
    }).expect("SDE NPC with fitted health layers and persistent shield damage").type_id;
    let stats = ShipStats::from_fit(&Fit::for_space_type(sde.clone(), target_type, vec![]), Key::Ship);
    let hp = stats.max;
    let resonance = stats.resonance.map(|layer| layer[0]);
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    for (layer, effect_id) in [EFFECT_NPC_REMOTE_SHIELD_BOOSTER, EFFECT_NPC_REMOTE_ARMOR_REPAIRER, EFFECT_NPC_REMOTE_HULL_REPAIRER].into_iter().enumerate() {
        let effect = super::super::super::registry::resolve(&sde, effect_id).expect("NPC repair effect");
        let source_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == effect_id)
            && supported_for_type(&sde, ty.type_id, &effect)
            && Fit::for_space_type(sde.clone(), ty.type_id, vec![]).value(Key::Ship, repair::amount_attribute(layer)) > 0.0)
            .expect("SDE NPC repair source").type_id;
        let system = 30_000 + layer as i64;
        let source = system * 100;
        let hostile = source + 1;
        let ally = source + 2;
        let rival = source + 3;
        let outsider = source + 4;
        world.system(system).await.unwrap();
        let (shot_tx, mut shot_rx) = tokio::sync::mpsc::unbounded_channel();
        world.with(system, move |s| {
            s.insert(ball(source, source_type, 1, None));
            s.insert(ball(hostile, target_type, 2, Some(1)));
            s.insert(ball(ally, target_type, 1, None));
            s.insert(ball(rival, target_type, 1, None));
            s.insert(ball(outsider, target_type, 2, None));
            assert!(s.add_npc(source, brain(Some(hostile), Some(shot_tx))));
            assert!(s.add_npc(ally, brain(None, None)));
            assert!(s.add_npc(rival, brain(None, None)));
            assert!(s.add_npc(outsider, brain(None, None)));
        }).await.unwrap();
        let generation = tokio::time::timeout(std::time::Duration::from_secs(10), shot_rx.recv())
            .await.expect("NPC shot timed out").expect("NPC callback closed");
        world.with(system, move |s| {
            let first = s.prepare_body_combat_source(source, None, Some(i64::from(effect_id)), None,
                crate::lua::events::resources::FactOrigin::Natural, move |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
            let second = s.prepare_body_combat_source(source, None, Some(i64::from(effect_id)), None,
                crate::lua::events::resources::FactOrigin::Natural, move |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
            assert_eq!(first.sequence.checked_add(1), Some(second.sequence));
            assert!(first.source_is_npc && first.actual_actor_character.is_none());
            s.finish_combat_attempt(&second);
            s.finish_combat_attempt(&first);
        }).await.unwrap();
        let full_source = world.ball(system, source).await.unwrap();
        let full_victim = world.ball(system, hostile).await.unwrap();
        run_source(&db, &world, system, &full_source, &full_victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
        let full = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
        assert_eq!(full, 1.0);
        let raw = (0..layer).map(|i| hp[i] / resonance[i]).sum::<f64>() + hp[layer] * 0.5 / resonance[layer];
        for id in [ally, rival, outsider] {
            let weak = Arc::downgrade(&world);
            let mut attempt = world.with(system, move |s| {
                let facts = s.prepare_body_combat_source(source, None, None, None,
                    crate::lua::events::resources::FactOrigin::Natural,
                    move |s| s.npc_projection_valid(source, hostile, generation)).unwrap();
                PreparedCombatAttempt::new(facts, weak, system)
            }).await.unwrap();
            let outcome = apply_damage_guarded(&db, &world, system, id, [raw, 0.0, 0.0, 0.0],
                crate::services::combat::damage::DamageSource { facts: Some(attempt.facts()), ..Default::default() }, false, |_| true).await.unwrap().unwrap();
            attempt.disarm();
            assert!(outcome.dealt > 0.0 && !outcome.lethal);
        }
        // Publish stored actor fractions, calibrating only if independently timed
        // shield recharge made the real damage produce unequal raw conditions.
        world.with(system, move |s| {
            let ids = [ally, rival, outsider];
            let raw = ids.map(|id| {
                let condition = s.combat_condition_snapshot(id).unwrap().condition;
                condition.now[layer] / condition.stats.max[layer]
            });
            if raw[0] != raw[1] || raw[0] != raw[2] {
                for id in ids { s.set_condition_fraction_mask(id, 1 << layer, 0.5); }
            }
            let mut fractions = [0.0; 3];
            for (index, id) in ids.into_iter().enumerate() {
                let condition = s.combat_condition_snapshot(id).unwrap().condition;
                fractions[index] = condition.now[layer] / condition.stats.max[layer];
                let mut health = s.ball(id).unwrap().health;
                health[layer] = fractions[index];
                s.set_health(id, health);
            }
            assert_eq!(fractions[0], fractions[1], "actor selection fractions must tie");
            assert_eq!(fractions[0], fractions[2], "wrong-owner fraction must match");
        }).await.unwrap();
        let before = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
        assert!(before < 1.0);
        assert!(world.after(system, CYCLE_WAIT_MS).await);
        let after_full = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
        assert_eq!(after_full, before, "a full target must not schedule a later repair");
        let me = world.ball(system, source).await.unwrap();
        let victim = world.ball(system, hostile).await.unwrap();
        let missing_type = sde.types.iter().find(|ty| ty.attribute(repair::amount_attribute(layer)).is_none())
            .expect("SDE type without the amount attribute").type_id;
        let mut missing = me.clone();
        missing.type_id = missing_type;
        assert!(!supported_for_type(&sde, missing_type, &effect));
        run_source(&db, &world, system, &missing, &victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
        assert!(world.after(system, CYCLE_WAIT_MS).await);
        let unchanged = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
        assert_eq!(unchanged, before);
        let source_fit = Fit::for_space_type(sde.clone(), source_type, vec![]);
        let optimal = effect.attribute(EffectAttribute::Range).map_or(0.0, |attr| source_fit.value(Key::Ship, attr));
        let falloff = effect.attribute(EffectAttribute::Falloff).map_or(0.0, |attr| source_fit.value(Key::Ship, attr));
        let base = source_fit.value(Key::Ship, repair::amount_attribute(layer));
        let facts = world.with(system, move |s| {
            let me = s.sampled_ball(source).unwrap();
            let a = s.sampled_ball(ally).unwrap();
            let b = s.sampled_ball(rival).unwrap();
            let other = s.sampled_ball(outsider).unwrap();
            let a_gap = gap(&me, &a);
            let b_gap = gap(&me, &b);
            let selected = s.assist_target(source, |me, candidate| {
                if candidate.id == hostile || me.mode == MODE_WARP || candidate.mode == MODE_WARP { return None; }
                score(npc_health_fraction(candidate, layer), gap(me, candidate), optimal, falloff)
            }).map(|ball| ball.id);
            (s.npc_projection_valid(source, hostile, generation),
                s.npc_assist_eligible(source, ally), s.npc_assist_eligible(source, rival),
                s.npc_assist_eligible(source, outsider), a.health[layer], b.health[layer],
                other.health[layer], a_gap, b_gap, selected)
        }).await.unwrap();
        let amount = repair::scaled_amount(base, facts.7, optimal, falloff);
        assert!(facts.0 && facts.1 && facts.2 && !facts.3, "projection/eligibility: {facts:?}");
        assert_eq!(facts.4, facts.5, "selection scores must tie: {facts:?}");
        assert_eq!(facts.4, facts.6, "outsider must have the same published health: {facts:?}");
        assert_eq!(facts.9, Some(ally), "lowest-ID selection: {facts:?}; optimal={optimal} falloff={falloff}");
        assert!(amount.is_finite() && amount > 0.0, "repair amount={amount} base={base} optimal={optimal} falloff={falloff} facts={facts:?}");
        let actor_before = world.with(system, move |s| s.combat_condition_snapshot(ally).unwrap().condition.now[layer]).await.unwrap();
        run_source(&db, &world, system, &me, &victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
        let (first, second) = world.with(system, move |s| (npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap(), npc_health_fraction(s.ball(rival).unwrap(), layer).unwrap())).await.unwrap();
        let actor_first = world.with(system, move |s| s.combat_condition_snapshot(ally).unwrap().condition.now[layer]).await.unwrap();
        if layer == 0 { assert!(actor_first > actor_before, "shield repair changes actor condition: before={actor_before} after={actor_first} amount={amount} facts={facts:?}"); }
        else { assert_eq!(actor_first, actor_before, "armor and hull do not repair at cycle start"); }
        if layer == 0 { assert!(first > before); } else { assert_eq!(first, before); }
        assert_eq!(second, before, "equal health ties choose the lower ID");
        let outsider_health = world.with(system, move |s| npc_health_fraction(s.ball(outsider).unwrap(), layer).unwrap()).await.unwrap();
        assert_eq!(outsider_health, before, "wrong-owner NPC must not be repaired");
        if layer != 0 {
            assert!(world.after(system, CYCLE_WAIT_MS).await);
            let after = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
            let actor_after = world.with(system, move |s| s.combat_condition_snapshot(ally).unwrap().condition.now[layer]).await.unwrap();
            assert!(actor_after > actor_before, "cycle end changes actor condition");
            assert!(after > before, "armor and hull repair at cycle end");
            // The remaining damaged ally is selected, then made ineligible before the timer.
            run_source(&db, &world, system, &me, &victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
            world.with(system, move |s| { s.spawn_group([source], |_| {}); }).await.unwrap();
            assert!(world.after(system, CYCLE_WAIT_MS).await);
            let unchanged = world.with(system, move |s| npc_health_fraction(s.ball(rival).unwrap(), layer).unwrap()).await.unwrap();
            assert_eq!(unchanged, before, "an ally leaving the support group cancels the repair");
            run_source(&db, &world, system, &me, &victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
            assert!(world.after(system, CYCLE_WAIT_MS).await);
            let no_ally = world.with(system, move |s| npc_health_fraction(s.ball(rival).unwrap(), layer).unwrap()).await.unwrap();
            assert_eq!(no_ally, before, "no eligible ally starts no repair");

            world.with(system, move |s| { s.spawn_group([source, rival], |_| {}); }).await.unwrap();
            run_source(&db, &world, system, &me, &victim, &effect, MOVE_CYCLE_MS, generation).await.ok().unwrap();
            world.with(system, move |s| {
                s.command(rival, |ball, _, _| {
                    ball.pos = [1.0e12, 0.0, 0.0];
                    ball.old_pos = ball.pos;
                    vec![]
                });
            }).await.unwrap();
            assert!(world.after(system, MOVE_WAIT_MS).await);
            let out_of_range = world.with(system, move |s| npc_health_fraction(s.ball(rival).unwrap(), layer).unwrap()).await.unwrap();
            assert_eq!(out_of_range, before, "moving out of range cancels the repair");

            world.with(system, move |s| {
                s.command(rival, |ball, _, _| {
                    ball.pos = [0.0; 3];
                    ball.old_pos = ball.pos;
                    vec![]
                });
            }).await.unwrap();
            assert!(world.after(system, 1_200).await);
            run_source(&db, &world, system, &me, &victim, &effect, 10_000.0, generation).await.ok().unwrap();
            world.with(system, move |s| { s.remove_ball(hostile); }).await.unwrap();
            assert!(world.after(system, 1_500).await);
            world.with(system, move |s| { s.insert(ball(hostile, target_type, 2, Some(1))); }).await.unwrap();
            assert!(world.after(system, 5_000).await);
            let new_generation = tokio::time::timeout(std::time::Duration::from_secs(10), shot_rx.recv())
                .await.expect("relocked NPC shot timed out").expect("NPC callback closed");
            assert_ne!(new_generation, generation, "the same hostile must be locked under a new generation");
            assert!(world.with(system, move |s| s.npc_projection_valid(source, hostile, new_generation)).await.unwrap());
            assert!(world.after(system, 4_000).await);
            let stale = world.with(system, move |s| npc_health_fraction(s.ball(rival).unwrap(), layer).unwrap()).await.unwrap();
            assert_eq!(stale, before, "a stale generation cannot repair even after relocking");
        }
        world.with(system, move |s| { for id in [source, hostile, ally, rival, outsider] { s.remove_ball(id); } }).await.unwrap();
    }
}
