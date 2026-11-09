//! Dedicated SDE smoke for successful and stale NPC ECM generations.
use super::*;
use crate::services::effects::registry;
use crate::space::{NpcBrain, NpcConfig, WarpState, MODE_STOP};

fn ball(id: i64, type_id: i32) -> Ball {
    Ball { id, type_id, owner_id: 1, corp_id: 1, alliance_id: -1, char_id: None, signature_radius: None,
        mode: MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn npc_ecm_requires_current_target_generation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let victim_type = sde.types.iter().filter(|ty| matches!(ty.category_id, Some(CATEGORY_SHIP | CATEGORY_ENTITY | CATEGORY_DRONE)))
        .filter_map(|ty| {
            let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
            primary(SENSORS.map(|attr| fit.value(Key::Ship, attr))).map(|(index, defense)| (ty.type_id, index, defense))
        }).min_by(|a, b| a.2.total_cmp(&b.2)).expect("SDE sensor-bearing victim");
    let (source_type, effect) = sde.types.iter().filter(|ty| ty.category_id == Some(CATEGORY_ENTITY)).find_map(|ty| {
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        ty.effects().iter().find_map(|row| registry::resolve(&sde, row.effect_id)
            .filter(|effect| effect.kind == EffectKind::Ecm
                && chance(fit.value(Key::Ship, JAM_STRENGTH[victim_type.1]), victim_type.2, 1.0) == 1.0
                && range_multiplier(0.0, effect.attribute(EffectAttribute::Range).map_or(0.0, |a| fit.value(Key::Ship, a)), effect.attribute(EffectAttribute::Falloff).map_or(0.0, |a| fit.value(Key::Ship, a))) > 0.0)
            .map(|effect| (ty.type_id, effect)))
    }).expect("SDE ECM source with guaranteed jam");
    let victim_type = victim_type.0;
    let world = World::new(db.clone(), sde, 1);
    let source = ball(-101, source_type);
    let mut victim = ball(-102, victim_type);
    victim.signature_radius = Some(1.0);
    world.add_ship(1, source.clone()).await.unwrap();
    world.add_ship(1, victim.clone()).await.unwrap();
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    world.with(1, move |s| {
        let mut brain = NpcBrain::new(None, NpcConfig {
            fly_range: 1.0, aggro_range: 1000.0, rate_of_fire_ms: 60000.0,
            scan_resolution: 1.0, repairs: (0.0, 0.0, 60000.0),
        }, Arc::new(move |_, action| {
            let events = events.clone();
            Some(Box::pin(async move { let _ = events.send(action); }))
        }));
        brain.target = Some((-102, 1, 0));
        brain.announced = true;
        assert!(s.add_npc(-101, brain));
    }).await.unwrap();
    let generation = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(crate::space::NpcAction::Shot { generation, .. }) = received.recv().await { break generation; }
        }
    }).await.expect("NPC shot generation");
    assert!(world.with(1, move |s| s.npc_projection_valid(-101, -102, generation)).await.unwrap());
    let fit = Fit::for_space_type(world.sde().clone(), source.type_id, Vec::new());
    let cx = SourceContext { db: &db, world: &world, system: 1, source: &source, target: &victim,
        module: -103, effect: &effect, duration_ms: 5000.0, fit: &fit, key: Key::Ship,
        guard: SourceGuard::Npc { source: -101, target: -102, generation }, attacker: None,
        drone: None, phase: DronePhase::End };
    apply_source(&cx).await.ok().unwrap();
    assert!(!combat::jamming::is_jammed(victim.id, crate::space::now_ms()));
    let cx = SourceContext { phase: DronePhase::Begin, ..cx };
    let mut failure = super::super::source(&fit, Key::Ship, source.id, -103, &effect, 5000.0);
    failure.strengths = [0.0; 4];
    assert!(apply_one(&db, &world, 1, &failure, &victim, None, Some(cx.guard)).await.ok().unwrap().is_some());
    assert!(!combat::jamming::is_jammed(victim.id, crate::space::now_ms()));
    let mut invalid_range = failure.clone();
    invalid_range.optimal = f64::NAN;
    assert!(apply_one(&db, &world, 1, &invalid_range, &victim, None, Some(cx.guard)).await.ok().unwrap().is_none());
    apply_source(&cx).await.ok().unwrap();
    assert!(combat::jamming::is_jammed(victim.id, crate::space::now_ms()));
    world.with(1, move |s| {
        combat::jamming::clear(s, -102);
        s.drop_npc_targets(-101);
        assert!(!s.npc_projection_valid(-101, -102, generation));
    }).await.unwrap();
    assert!(apply_one(&db, &world, 1, &failure, &victim, None, Some(cx.guard)).await.ok().unwrap().is_none());
    apply_source(&cx).await.ok().unwrap();
    assert!(!combat::jamming::is_jammed(victim.id, crate::space::now_ms()));
}
