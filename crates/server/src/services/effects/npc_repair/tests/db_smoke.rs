use super::*;
use std::sync::Arc;
use crate::{consts::{dogma::ATTR_ARMOR_HP, dogma::ATTR_SHIELD_CAPACITY, dogma::ATTR_STRUCTURE_HP,
    dogma::ATTR_SHIELD_EM_DAMAGE_RESONANCE, dogma::ATTR_ARMOR_EM_DAMAGE_RESONANCE,
    dogma::ATTR_STRUCTURE_EM_DAMAGE_RESONANCE, CATEGORY_ENTITY},
    services::combat::groups_health::damage,
    space::{NpcBrain, NpcConfig, WarpState, MODE_STOP}};

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
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    }
}

fn brain(hostile: Option<i64>, shots: Option<tokio::sync::mpsc::UnboundedSender<u64>>) -> NpcBrain {
    let mut brain = NpcBrain::new(hostile.map(|_| crate::space::HOSTILE_TO_ALL), NpcConfig {
        fly_range: 1.0, aggro_range: 1000.0, rate_of_fire_ms: 1e9,
        scan_resolution: 17_163.0, repairs: (0.0, 0.0, 1000.0),
    }, Arc::new(move |action| {
        if let crate::space::NpcAction::Shot { generation, .. } = action {
            if let Some(tx) = &shots { let _ = tx.send(generation); }
        }
        Box::pin(async {})
    }));
    if let Some(id) = hostile {
        brain.target = Some((id, 1, 0));
        brain.announced = true;
    }
    brain
}
async fn run_source(db: &crate::db::Db, world: &Arc<crate::space::World>, system: i64, me: &Ball, victim: &Ball, effect: &Effect, duration_ms: f64, generation: u64) -> Result<(), CallError> {
    let fit = crate::services::fit::Fit::for_space_type(world.sde().clone(), me.type_id, vec![]);
    apply_source(&SourceContext { db, world, system, source: me, target: victim, module: 0, effect, duration_ms, fit: &fit,
        key: crate::services::fit::Key::Ship, guard: SourceGuard::Npc { source: me.id, target: victim.id, generation },
        attacker: None, drone: None, phase: DronePhase::Begin }).await
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_repair_real_sde_phase_selection_and_guards() {
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let target_type = sde.types.iter().find(|ty| {
        ty.category_id == Some(CATEGORY_ENTITY)
            && [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].into_iter()
                .all(|id| ty.attribute(id).is_some_and(|v| v.is_finite() && v > 0.0))
            && [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE].into_iter()
                .all(|id| ty.attribute(id).unwrap_or(1.0) > 0.0)
    }).expect("SDE NPC with all health layers").type_id;
    let hp = [ATTR_SHIELD_CAPACITY, ATTR_ARMOR_HP, ATTR_STRUCTURE_HP].map(|id| sde.types.attribute(target_type, id).unwrap());
    let resonance = [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_ARMOR_EM_DAMAGE_RESONANCE, ATTR_STRUCTURE_EM_DAMAGE_RESONANCE]
        .map(|id| sde.types.attribute(target_type, id).unwrap_or(1.0));
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    for (layer, effect_id) in [EFFECT_NPC_REMOTE_SHIELD_BOOSTER, EFFECT_NPC_REMOTE_ARMOR_REPAIRER, EFFECT_NPC_REMOTE_HULL_REPAIRER].into_iter().enumerate() {
        let effect = super::super::super::registry::resolve(&sde, effect_id).expect("NPC repair effect");
        let source_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == effect_id)
            && supported_for_type(&sde, ty.type_id, &effect)
            && ty.attribute(repair::amount_attribute(layer)).is_some_and(|v| v > 0.0))
            .expect("SDE NPC repair source").type_id;
        let system = -30_000 - layer as i64;
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
            s.add_npc(source, brain(Some(hostile), Some(shot_tx)));
            s.add_npc(ally, brain(None, None));
            s.add_npc(rival, brain(None, None));
            s.add_npc(outsider, brain(None, None));
        }).await.unwrap();
        let generation = tokio::time::timeout(std::time::Duration::from_secs(10), shot_rx.recv())
            .await.expect("NPC shot timed out").expect("NPC callback closed");
        let full_source = world.ball(system, source).await.unwrap();
        let full_victim = world.ball(system, hostile).await.unwrap();
        run_source(&db, &world, system, &full_source, &full_victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
        let full = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
        assert_eq!(full, 1.0);
        let raw = (0..layer).map(|i| hp[i] / resonance[i]).sum::<f64>() + hp[layer] * 0.5 / resonance[layer];
        for id in [ally, rival, outsider] { damage(&db, &world, system, id, [raw, 0.0, 0.0, 0.0], None, None).await.ok().unwrap(); }
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
        run_source(&db, &world, system, &me, &victim, &effect, CYCLE_MS, generation).await.ok().unwrap();
        let (first, second) = world.with(system, move |s| (npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap(), npc_health_fraction(s.ball(rival).unwrap(), layer).unwrap())).await.unwrap();
        if layer == 0 { assert!(first > before); } else { assert_eq!(first, before); }
        assert_eq!(second, before, "equal health ties choose the lower ID");
        let outsider_health = world.with(system, move |s| npc_health_fraction(s.ball(outsider).unwrap(), layer).unwrap()).await.unwrap();
        assert_eq!(outsider_health, before, "wrong-owner NPC must not be repaired");
        if layer != 0 {
            assert!(world.after(system, CYCLE_WAIT_MS).await);
            let after = world.with(system, move |s| npc_health_fraction(s.ball(ally).unwrap(), layer).unwrap()).await.unwrap();
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
