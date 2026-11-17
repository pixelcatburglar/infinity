//! Real-SDE regression: a behavior-carried NPC shield booster repairs on the
//! existing shared local cycle, not the legacy entity attributes.
use super::*;
use crate::services::combat::tests::remote_repair_db::{ball, condition};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn behavior_shield_repair_uses_real_sde_rate() {
    const NPC_ID: i64 = i64::MIN + 561_899;
    const NPC_TYPE: i32 = 56189; // Stormbringer Disparu Troop.
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let data = sde.types.get(NPC_TYPE).expect("SDE carries the NPC type");
    assert!(data.effects().iter().any(|entry| entry.effect_id == infinity_core::consts::dogma::EFFECT_NPC_BEHAVIOR_SHIELD_BOOST));
    assert!(data.attribute(ATTR_ENTITY_SHIELD_BOOST_AMOUNT).is_none());
    assert!(data.attribute(ATTR_ENTITY_SHIELD_BOOST_DURATION).is_none());
    let amount = data.attribute(ATTR_BEHAVIOR_SHIELD_BOOSTER_AMOUNT).expect("behavior amount");
    let duration = data.attribute(ATTR_BEHAVIOR_SHIELD_BOOSTER_DURATION).expect("behavior duration");
    assert_eq!((amount, duration), (220.0, 6000.0));
    let expected = amount * 5000.0 / duration;
    let stats = npc_stats(&db, &sde, NPC_TYPE).await.unwrap();
    assert_eq!(stats.repairs.2, 5000.0, "shared cycle keeps the existing 5s floor");
    assert!((stats.repairs.0 - expected).abs() < 1e-9 * expected, "shield per shared cycle, got {}", stats.repairs.0);
    assert_eq!(stats.repairs.1, 0.0, "no armor repair source on this type");
    // Live path: real spawn_npc brain -> think -> effects FIFO -> npc_repair.
    let system = 1;
    let world = World::new(db.clone(), sde.clone(), 1);
    let mut npc = ball(NPC_ID, NPC_TYPE, 1_000_127);
    npc.char_id = None;
    world.add_npc_ship(system, npc).await.unwrap();
    let fit = Fit::for_space_type(sde.clone(), NPC_TYPE, Vec::new());
    let ship_stats = ShipStats::from_fit(&fit, Key::Ship);
    let before = [ship_stats.max[0] * 0.5, ship_stats.max[1], ship_stats.max[2]]; // remaining HP: damaged shield, full armor and hull.
    world.with(system, move |s| s.install_condition(NPC_ID, Condition {
        now: before, stats: ship_stats, shield_at: filetime_now() + 60 * FILETIME_SECOND,
    }, fit)).await.unwrap();
    spawn_npc(db.clone(), world.clone(), system, NPC_ID, NPC_TYPE, None).await.unwrap();
    let test_world = world.clone();
    let result = tokio::spawn(async move {
        // Poll through the actor every 100 ms; 4 s covers the staggered first
        // think (under 1 s) and its worker turn, well inside one 5 s cycle.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(4);
        let repaired = loop {
            assert!(test_world.after(system, 100).await, "system actor stays alive");
            let current = condition(&test_world, system, NPC_ID).await;
            if current.now[0] != before[0] { break current; }
            assert!(tokio::time::Instant::now() < deadline, "no NPC repair within 4 s");
        };
        assert!((repaired.now[0] - (before[0] + expected)).abs() < 1e-9 * expected, "first repair gains exactly one scaled cycle");
        assert_eq!(repaired.now[1..], before[1..], "armor and hull stay full");
        let health = test_world.with(system, move |s| s.ball(NPC_ID).unwrap().health).await.unwrap();
        assert!((health[0] - repaired.now[0] / repaired.stats.max[0]).abs() < 1e-6, "Ball.health follows the repair");
        assert_eq!(health[1..], [1.0, 1.0], "armor and hull health stay full");
    }).await;
    world.remove_ball(system, NPC_ID).await;
    result.unwrap();
}

/// The behavior rescale preserves the SDE nominal rate on the shared cycle:
/// shorter and longer durations both scale; invalid input admits nothing.
#[test]
fn behavior_cycle_amount_table() {
    const C: f64 = 5_000.0;
    for (amount, duration, expected) in [
        (160.0, 5_000.0, 160.0),              // duration == C: unchanged
        (220.0, 6_000.0, 220.0 * C / 6_000.0), // duration > C: shrinks
        (300.0, 2_000.0, 300.0 * C / 2_000.0), // duration < C: grows
    ] {
        assert_eq!(behavior_cycle_amount(Some(amount), Some(duration), C), Some(expected));
    }
    // Two layers with different own durations scale independently.
    assert_eq!(behavior_cycle_amount(Some(4_500.0), Some(9_000.0), C), Some(2_500.0));
    // Missing, non-positive or non-finite inputs admit no behavior amount.
    for (amount, duration) in [(None, Some(5_000.0)), (Some(220.0), None),
        (Some(0.0), Some(5_000.0)), (Some(-1.0), Some(5_000.0)), (Some(f64::NAN), Some(5_000.0)),
        (Some(220.0), Some(0.0)), (Some(220.0), Some(-1.0)), (Some(220.0), Some(f64::INFINITY))] {
        assert_eq!(behavior_cycle_amount(amount, duration, C), None);
    }
}
