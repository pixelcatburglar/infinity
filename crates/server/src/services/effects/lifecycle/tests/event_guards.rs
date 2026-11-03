use super::*;
use crate::space::{Ball, WarpState, MODE_STOP};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE"]
async fn target_loss_requires_live_source_and_current_generation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, crate::consts::dogma::EFFECT_CARGO_SCAN).unwrap();
    let world = World::new(db.clone(), sde, 1);
    let (notify, _) = tokio::sync::mpsc::unbounded_channel();
    let module = -987_654_321;
    let generation = Arc::new(AtomicBool::new(false));
    let vacant = {
        let mut st = state();
        if st.active.contains_key(&module) || st.activation_generation.contains_key(&module) {
            false
        } else {
            st.active.insert(module, ActiveModule { char_id: 1, stop: generation.clone(), forced_inactive: false });
            st.activation_generation.insert(module, generation.clone());
            true
        }
    };
    assert!(vacant, "fixture module key already in use");
    let result = tokio::spawn(async move {
        let cx = Ctx { db, world, notify, char_id: 1, ship_id: 2, system_id: 1 };
        let cycle = Cycle { cx, modules: vec![Module { id: module, type_id: 1, flag: 0 }], effect,
            target: Some(3), repeat: 1, stop: generation.clone(),
            owner: ActivationOwner { master: module, generation: generation.clone(), armed: true },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1.0, nominal_start: 0, other: None, repair_amount: 0.0 };
        let ball = |id, char_id| Ball {
            id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id,
            signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 0.0,
            agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, follow_id: 0, follow_range: 0.0,
            free: true, global: false, interactive: false, massive: false, warp: WarpState::default(),
            warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
            cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
        };
        let mut system = System::default();
        assert!(!cycle_source_current(&system, &cycle));
        assert!(!cycle_target_lost(&system, &cycle));
        system.insert(ball(2, Some(4)));
        assert!(!cycle_source_current(&system, &cycle));
        assert!(!cycle_target_lost(&system, &cycle));
        let mut system = System::default();
        system.insert(ball(2, Some(1)));
        assert!(cycle_source_current(&system, &cycle));
        assert!(cycle_target_lost(&system, &cycle));
        system.insert(ball(3, None));
        assert!(!cycle_target_lost(&system, &cycle));
        let mut system = System::default();
        system.insert(ball(2, Some(1)));
        generation.store(true, Ordering::Relaxed);
        assert!(!cycle_source_current(&system, &cycle));
        assert!(!cycle_target_lost(&system, &cycle));
        generation.store(false, Ordering::Relaxed);
        state().active.get_mut(&module).unwrap().stop = Arc::new(AtomicBool::new(false));
        assert!(!cycle.stop.load(Ordering::Relaxed));
        assert!(!cycle_source_current(&system, &cycle));
        assert!(!cycle_target_lost(&system, &cycle));
    }).await;
    {
        let mut st = state();
        st.active.remove(&module);
        st.activation_generation.remove(&module);
    }
    result.unwrap();
}
