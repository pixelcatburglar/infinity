use super::*;
use crate::space::{Ball, WarpState, MODE_STOP};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE"]
async fn target_loss_requires_live_source_and_current_generation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_CARGO_SCAN).unwrap();
    let world = World::new(db.clone(), sde, 1);
    let (notify, _) = tokio::sync::mpsc::unbounded_channel();
    let module = 2;
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
        let sde = cx.world.sde().clone();
        let module_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| i64::from(e.effect_id) == effect.id)).unwrap().type_id;
        let mut cycle = Cycle { cx, modules: vec![Module { id: module, type_id: module_type, flag: 0 }], presented_effect: effect.id, effect,
            target: Some(3), repeat: 1, stop: generation.clone(),
            owner: ActivationOwner { master: module, generation: generation.clone(), armed: true, combat_generations: Vec::new(), pending_combat: None, world: std::sync::Weak::new(), system: 0 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
            source_membership: None, target_type: None, inputs: crate::services::effects::parameters::CycleInputs::default() };
        let ball = |id, char_id| Ball {
            id, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1, char_id,
            signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1.0, max_vel: 0.0,
            agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3],
            old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None, follow_id: 0, follow_range: 0.0,
            free: true, global: false, interactive: false, massive: false, warp: WarpState::default(),
            last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
            cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
        };
        let install = |system: &mut System| {
            let fit = crate::services::fit::attributes::tests::module_fit(sde.clone(), 1, module_type, true);
            let stats = crate::services::combat::groups_health::ShipStats::from_fit(&fit, Key::Ship);
            system.install_condition(2, crate::services::combat::groups_health::Condition { now: stats.max, stats, shield_at: 0 }, fit);
            system.combat_membership_generation(2).unwrap()
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
        cycle.source_membership = Some(install(&mut system));
        assert!(source_subject_current(&mut system, 2, 1, cycle.source_membership.unwrap(), &cycle.modules[0], cycle.presented_effect, &generation));
        system.insert(ball(3, None));
        assert!(!cycle_target_lost(&system, &cycle));
        let mut system = System::default();
        system.insert(ball(2, Some(1)));
        generation.store(true, Ordering::Relaxed);
        assert!(!cycle_source_current(&system, &cycle));
        assert!(!cycle_target_lost(&system, &cycle));
        cycle.source_membership = Some(install(&mut system));
        assert!(source_subject_current(&mut system, 2, 1, cycle.source_membership.unwrap(), &cycle.modules[0], cycle.presented_effect, &generation));
        generation.store(false, Ordering::Relaxed);
        state().active.get_mut(&module).unwrap().stop = Arc::new(AtomicBool::new(false));
        assert!(!cycle.stop.load(Ordering::Relaxed));
        assert!(!source_subject_current(&mut system, 2, 1, cycle.source_membership.unwrap(), &cycle.modules[0], cycle.presented_effect, &generation));
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
