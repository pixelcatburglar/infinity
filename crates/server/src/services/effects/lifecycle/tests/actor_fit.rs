use super::*;
#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn cycle_source_gate_consumes_deadline_and_binds_membership() {
    use crate::services::effects::tests::fixture;
    use crate::services::combat::groups_health::{Condition, ShipStats};
    use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, dogma::ATTR_CAPACITOR_CAPACITY};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let effect = registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_CARGO_SCAN).unwrap();
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.attribute(ATTR_CAPACITOR_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let module_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE)
        && ty.effects().iter().any(|row| i64::from(row.effect_id) == effect.id)).unwrap().type_id;
    let fresh = || crate::services::fit::attributes::tests::module_fit(sde.clone(), hull, module_type, true);
    let mut fit = fresh();
    assert!(fit.set_module_active(2, true));
    let (fit, active, expired) = fixture::due_fit(fit, &sde, |fit| fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY));
    let stats = ShipStats::from_fit(&fit, Key::Ship);
    let mut system = System::default();
    system.add_persistent_ship(fixture::ball(1, hull, 1, Some(1), 0.0));
    system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, fit);
    let membership = system.combat_membership_generation(1).unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let (notify, _) = tokio::sync::mpsc::unbounded_channel();
    let cx = Ctx { db, world, notify, char_id: 1, ship_id: 1, system_id: 1 };
    let stop = Arc::new(AtomicBool::new(false));
    assert!(state().active.insert(2, ActiveModule { char_id: 1, stop: stop.clone(), forced_inactive: false }).is_none());
    state().activation_generation.insert(2, stop.clone());
    let result = crate::tests::catch_scenario(async {
        let mut cycle = Cycle { cx, modules: vec![Module { id: 2, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }],
            effect, target: None, repeat: 1, stop: stop.clone(),
            owner: ActivationOwner { master: 2, generation: stop.clone(), armed: false, combat_generations: vec![],
                pending_combat: None, world: std::sync::Weak::new(), system: 1 },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0,
            other: None, repair_amount: 0.0, illegal: false, source_membership: Some(membership),
            target_type: None, inputs: super::super::super::parameters::CycleInputs::default() };
        assert_eq!(system.combat_fit_revision(1), Some(0));
        assert!(capture_cycle_inputs(&mut system, &mut cycle).unwrap());
        let frame = cycle.inputs.frame.as_ref().unwrap();
        assert_eq!(frame.capacitor.0, expired);
        assert_ne!(frame.capacitor.0, active);
        crate::services::combat::groups_health::fill_capacitor(1, frame.capacitor.0, frame.capacitor.1, crate::client::filetime_now());
        assert!(crate::services::combat::groups_health::drain_capacitor(&cycle.cx,
            frame.capacitor.0, frame.capacitor.1, expired / 2.0));
        let fraction = crate::services::combat::groups_health::npc_capacitor_fraction(&sde, system.ball(1).unwrap()).unwrap();
        assert!((fraction - 0.5).abs() < 0.01);
        let current = |s: &mut System, owner, module: &Module, generation: &Arc<AtomicBool>|
            source_subject_current(s, 1, owner, membership, module, cycle.effect.id, generation);
        assert!(current(&mut system, 1, &cycle.modules[0], &stop));
        assert!(!current(&mut system, 2, &cycle.modules[0], &stop));
        let missing = Module { id: 3, type_id: module_type, flag: FLAG_HI_SLOT_FIRST };
        assert!(!current(&mut system, 1, &missing, &stop));
        assert!(system.fitted_module_online(1, 2, false));
        assert!(!current(&mut system, 1, &cycle.modules[0], &stop));
        assert!(system.fitted_module_online(1, 2, true));
        assert!(!current(&mut system, 1, &cycle.modules[0], &Arc::new(AtomicBool::new(false))));
        stop.store(true, Ordering::Relaxed);
        assert!(current(&mut system, 1, &cycle.modules[0], &stop));
        state().active.get_mut(&2).unwrap().forced_inactive = true;
        assert!(!current(&mut system, 1, &cycle.modules[0], &stop));
        state().active.get_mut(&2).unwrap().forced_inactive = false;
        stop.store(false, Ordering::Relaxed);
        system.remove_ball(1);
        system.add_persistent_ship(fixture::ball(1, hull, 1, Some(1), 0.0));
        let fit = fresh();
        let stats = ShipStats::from_fit(&fit, Key::Ship);
        system.install_condition(1, Condition { now: stats.max, stats, shield_at: 0 }, fit);
        assert_ne!(system.combat_membership_generation(1), Some(membership));
        assert!(!current(&mut system, 1, &cycle.modules[0], &stop));
        assert!(!capture_cycle_inputs(&mut system, &mut cycle).unwrap());
    }).await;
    state().active.remove(&2);
    state().activation_generation.remove(&2);
    crate::services::combat::forget_ship(1);
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn activated_self_buff_retracks_member_cycle_values() {
    use crate::services::effects::tests::fixture;
    use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST, dogma::EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS};
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let skill = sde.dogma.effect(EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS).unwrap()
        .modifiers().iter().find(|m| m.func.as_str() == "LocationRequiredSkillModifier").and_then(|m| m.skill).unwrap();
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.published == Some(true)).unwrap().type_id;
    let turret = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == infinity_core::consts::dogma::EFFECT_TARGET_ATTACK)
        && sde.skill_plans.requirements(t.type_id).iter().any(|&(required, _)| required == skill)).unwrap().type_id;
    let buff_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS)).unwrap().type_id;
    let mut f = crate::services::tests::fixture::create_ship(&db, hull, crate::services::tests::fixture::ShipLocation::Docked { station: 1 }, &[crate::services::tests::fixture::ShipItemSpec { type_id: buff_type, flag_id: FLAG_MED_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let guns = vec![db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&turret, &f.character, &f.ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id"),
        db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&turret, &f.character, &f.ship, &(FLAG_HI_SLOT_FIRST + 1)]).await.unwrap().get("item_id")];
    f.items.extend(guns.iter().copied());
    let fit = Fit::load(&db, sde.clone(), f.character, f.ship).await.unwrap();
    let mut system = System::default();
    system.add_persistent_ship(fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0));
    system.install_condition(f.ship, crate::services::combat::prepare_condition_with_fit(&db, f.ship, &fit).await.unwrap().into_condition(), fit);
    let cx = Ctx { db: db.clone(), world: World::new(db.clone(), sde.clone(), 1), notify: tokio::sync::mpsc::unbounded_channel().0, char_id: f.character, ship_id: f.ship, system_id: 1 };
    let buff_stop = Arc::new(AtomicBool::new(false));
    state().active.insert(module_id, ActiveModule { char_id: f.character, stop: buff_stop.clone(), forced_inactive: false });
    let mut cycle = Cycle { cx, modules: guns.iter().enumerate().map(|(i, &id)| Module { id, type_id: turret, flag: FLAG_HI_SLOT_FIRST + i as i32 }).collect(),
        effect: registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_TARGET_ATTACK).unwrap(), target: None, repeat: 1, stop: Arc::new(AtomicBool::new(false)),
        owner: ActivationOwner { master: guns[0], generation: Arc::new(AtomicBool::new(false)), armed: false, combat_generations: vec![], pending_combat: None, world: std::sync::Weak::new(), system: 1 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
        source_membership: None, target_type: None, inputs: parameters::CycleInputs::default() };
    let params = |cycle: &Cycle| cycle.inputs.members.iter().map(|member| match &member.packet {
        parameters::MemberPacket::Turret(turret) => turret.parameters, _ => panic!("member packet is not turret") }).collect::<Vec<_>>();
    let base = { assert!(capture_cycle_inputs(&mut system, &mut cycle).unwrap()); params(&cycle) };
    let mut buff = Cycle { cx: cycle.cx.clone(), modules: vec![Module { id: module_id, type_id: buff_type, flag: FLAG_MED_SLOT_FIRST }],
        effect: registry::resolve(&sde, EFFECT_GUNNERY_MAX_RANGE_FALLOFF_TRACKING_SPEED_BONUS).unwrap(), target: None, repeat: 1, stop: buff_stop.clone(),
        owner: ActivationOwner { master: module_id, generation: buff_stop.clone(), armed: true, combat_generations: vec![], pending_combat: None, world: std::sync::Weak::new(), system: 1 },
        started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
        source_membership: None, target_type: None, inputs: parameters::CycleInputs::default() };
    let revision = system.combat_fit_revision(f.ship);
    assert!(capture_cycle_inputs(&mut system, &mut buff).unwrap() && system.combat_fit_revision(f.ship) == revision);
    assert!(capture_cycle_inputs(&mut system, &mut cycle).unwrap() && params(&cycle) == base);
    assert!(system.fitted_module_active(f.ship, module_id, true));
    assert!(capture_cycle_inputs(&mut system, &mut cycle).unwrap());
    assert!(params(&cycle).iter().zip(&base).all(|(new, old)| new.0 > old.0 && new.1 > old.1 && new.2 > old.2),
        "active buff did not raise every member's tracking, optimal and falloff");
    assert!(system.fitted_module_active(f.ship, module_id, false));
    assert!(capture_cycle_inputs(&mut system, &mut cycle).unwrap());
    assert_eq!(params(&cycle), base);
    crate::services::tests::fixture::cleanup_ship(&db, None, f).await.unwrap();
}
