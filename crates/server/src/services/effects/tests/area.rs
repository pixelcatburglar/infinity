//! Smartbomb and bomb damage through real fitted effects and the system actor.
use super::fixture;
use infinity_core::{consts::{dogma::ATTR_EM_DAMAGE, dogma::ATTR_SHIELD_CAPACITY, dogma::ATTR_SHIELD_EM_DAMAGE_RESONANCE, CATEGORY_MODULE, CATEGORY_SHIP, dogma::EFFECT_EMP_WAVE, FLAG_HI_SLOT_FIRST}};
use crate::{dao::items::dogma, services::{combat::{self, ActiveModule, Ctx, groups_health::npc_health_fraction}, effects::{aoe, lifecycle::{ActivationOwner, Cycle, Module}, registry}, fit::Fit}, space::World};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn smartbomb_damages_near_owned_unpiloted_ball_but_not_source_or_outside() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 10.0)
        && t.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).is_some_and(|v| v > 0.0)).expect("shielded hull").type_id;
    let weapon = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_EMP_WAVE)).expect("smartbomb module").type_id;
    let effect = registry::resolve(&sde, EFFECT_EMP_WAVE).expect("smartbomb effect");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, weapon, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let near = fixture::item(&db, hull, f.character, 1, 0, 1, true, false).await.unwrap();
    let outside = fixture::item(&db, hull, f.character, 1, 0, 1, true, false).await.unwrap();
    let hub_id = crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed);
    let result = tokio::spawn(async move {
        dogma::set_override(&test_db, f.module, infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE, 100.0).await.unwrap();
        for attr in combat::ATTR_DAMAGE { dogma::set_override(&test_db, f.module, attr, if attr == ATTR_EM_DAMAGE { 1.0 } else { 0.0 }).await.unwrap(); }
        test_world.system(1).await.unwrap();
        test_world.with(1, |s| crate::space::snapshot::facts_test_init(s, 3, 4)).await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        fixture::admit(&test_db, &test_world, 1, fixture::ball(near, hull, f.character, None, 50.0)).await;
        fixture::admit(&test_db, &test_world, 1, fixture::ball(outside, hull, f.character, None, 150.0)).await;
        let system = crate::dao::sovereignty::systems::SystemRow {
            system_id: 1, hub_item_id: Some(hub_id), claim_corporation_id: None, alliance_id: None,
            claim_time: None, ownership_epoch: 0, revision: 1,
        };
        let hub = crate::dao::sovereignty::structures::HubRow {
            system: system.clone(),
            structure: crate::dao::sovereignty::structures::StructureRow {
                item_id: hub_id, system_id: 1, source_item_id: hub_id + 1,
                deployer_character_id: f.character, deployer_corporation_id: 1, deployed_at: 1,
                content_namespace: "test".into(), content_version: "test".into(),
                self_destruct_due_at: None, self_destruct_actor: None,
            },
            type_id: sde.sovereignty.hub().type_id, owner_id: sde.sovereignty.hub().deploy.default_owner,
            x: Some(75.0), y: Some(0.0), z: Some(0.0),
        };
        let capture = crate::dao::sovereignty::capture::CaptureRow {
            target_item_id: hub_id, system_id: 1, ownership_epoch: 0, controlling_team: None,
            scoring_team: None, defender_team: infinity_core::consts::OWNER_UNOWNED, score: 0.0, rate: 0.0,
            blocked: false, score_sim_time: crate::client::filetime_now(), checkpoint_wall_time: 0,
            revision: 0, capture_time_seconds: 10.0, defensive_regen_rate: 0.0, checkpoint_interval_ms: 1,
            content_namespace: "test".into(), content_version: "test".into(),
        };
        crate::space::sovereignty::publish(&test_world, crate::services::sovereignty::CommittedPublication {
            hubs: vec![crate::services::sovereignty::HubPublication { system, hub: Some(hub), capture: Some(capture) }],
            ..Default::default()
        });
        test_world.with(1, move |s| {
            assert!(s.is_damage_immune(hub_id));
            assert!(s.add_npc(hub_id, crate::space::NpcBrain::new(None,
                crate::space::NpcConfig { fly_range: 1.0, aggro_range: 0.0, rate_of_fire_ms: 1e9,
                    scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0) },
                Arc::new(|_, _| Some(Box::pin(async {}))))));
            assert!(matches!(s.combat_phase(hub_id), Some(crate::space::combat::CombatPhase::Live)));
        }).await.unwrap();
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("load fit");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: f.module, generation, armed: true,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id: f.module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        let source_before = test_world.ball(1, f.ship).await.unwrap().health;
        let hub_before = test_world.ball(1, hub_id).await.unwrap().health;
        aoe::start(&cx, &fit, &mut cycle).await.expect("smartbomb cycle");
        let near_ball = test_world.ball(1, near).await.unwrap();
        let far_ball = test_world.ball(1, outside).await.unwrap();
        assert!(npc_health_fraction(&near_ball, 0).unwrap() < 1.0);
        assert_eq!(npc_health_fraction(&far_ball, 0), Some(1.0));
        assert_eq!(test_world.ball(1, f.ship).await.unwrap().health, source_before);
        assert_eq!(test_world.ball(1, hub_id).await.unwrap().health, hub_before);
        test_world.with(1, move |s| { s.set_ball_position(outside, [75.0, 0.0, 0.0], |_, _| true); }).await.unwrap();
        assert!(test_world.after(1, 1500).await);
        assert_eq!(test_world.with(1, move |s| s.sample_position(outside)).await.flatten(), Some([75.0, 0.0, 0.0]));
        let before_full = (test_world.ball(1, near).await.unwrap().health, test_world.ball(1, outside).await.unwrap().health);
        assert!(matches!(aoe::start(&cx, &fit, &mut cycle).await, Err(crate::services::CallError::User { msg, .. })
            if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY));
        assert_eq!((test_world.ball(1, near).await.unwrap().health, test_world.ball(1, outside).await.unwrap().health), before_full);
        assert_eq!(test_world.ball(1, hub_id).await.unwrap().health, hub_before);
        let frame = test_world.with(1, |s| crate::space::snapshot::capture_fact_checkpoint(s).unwrap().unwrap()).await.unwrap();
        for captured in frame.events() {
            if let crate::lua::events::GameEvent::DamageApplied { target, .. } = captured.event {
                assert_ne!(target.item_id, hub_id);
                assert_ne!(target.item_id, outside);
            }
        }
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    for id in [f.ship, near, outside, hub_id] { world.remove_ball(1, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn bomb_flies_before_dealing_raw_damage_to_launcher_and_consumes_one_charge() {
    use infinity_core::consts::{dogma::ATTR_EXPLOSION_DELAY, dogma::ATTR_MAX_VELOCITY, dogma::ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE, dogma::ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, dogma::ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, CATEGORY_CHARGE};
    use crate::services::{combat::targeting_charges::loaded, effects::bombs, fit::Key};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let resonances = [ATTR_SHIELD_EM_DAMAGE_RESONANCE, ATTR_SHIELD_THERMAL_DAMAGE_RESONANCE, ATTR_SHIELD_KINETIC_DAMAGE_RESONANCE, ATTR_SHIELD_EXPLOSIVE_DAMAGE_RESONANCE];
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 0.0)
        && resonances.iter().all(|&attr| t.attribute(attr).is_some_and(|v| v > 0.0))).expect("susceptible shielded hull").type_id;
    let launcher = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.group_id == Some(862)).expect("bomb launcher");
    let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
        .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| launcher.attribute(a.attribute_id)).collect();
    let charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)
        && t.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
        && t.attribute(ATTR_EXPLOSION_DELAY).is_some_and(|v| v >= 3000.0 && v <= 12000.0)
        && t.attribute(ATTR_MAX_VELOCITY).is_some_and(|v| v > 0.0)
        && t.attribute(infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE).is_some_and(|v| v > 0.0)
        && combat::ATTR_DAMAGE.iter().any(|&a| t.attribute(a).is_some_and(|v| v > 0.0))).expect("compatible damaging bomb");
    let weapon = launcher.type_id;
    let ammo = charge.type_id;
    let effect = launcher.effects().iter().find_map(|e| registry::resolve(&sde, e.effect_id)).expect("launcher effect");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, weapon, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let second_victim = crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed);
    let result = tokio::spawn(async move {
        const SHIELD: f64 = 1_000_000_000.0;
        dogma::set_override(&test_db, f.ship, ATTR_SHIELD_CAPACITY, SHIELD).await.unwrap();
        for attr in resonances {
            dogma::set_override(&test_db, f.ship, attr, 1.0).await.unwrap();
        }
        test_db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,2)", &[&f.ship,&FLAG_HI_SLOT_FIRST,&ammo]).await.unwrap();
        test_world.system(1).await.unwrap();
        test_world.with(1, |s| crate::space::snapshot::facts_test_init(s, 3, 4)).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("load fit");
        let key = Key::Charge(f.module);
        let delay = fit.value(key, ATTR_EXPLOSION_DELAY);
        let velocity = fit.value(key, ATTR_MAX_VELOCITY);
        let radius = fit.value(key, infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE);
        let raw: f64 = combat::ATTR_DAMAGE.iter().map(|&a| fit.value(key, a).max(0.0)).sum();
        let fitted_resonances = resonances.map(|attr| fit.value(Key::Ship, attr));
        let inputs = format!("hull={hull} ammo={ammo} delay_ms={delay} velocity={velocity} radius={radius} raw={raw} shield_resonances={fitted_resonances:?}");
        let nominal_travel = velocity * crate::space::ms_to_seconds(delay);
        let intended_launcher_position = [0.0, 0.0, -nominal_travel * 0.75];
        eprintln!("bomb fitted inputs: {inputs}; nominal_travel={nominal_travel}, old_midpoint_distance={}, old_midpoint_radius_gap={}, interior_distance={}, interior_radius_gap={}, intended_launcher_position={intended_launcher_position:?}",
            nominal_travel * 0.5, radius - nominal_travel * 0.5, nominal_travel * 0.25, radius - nominal_travel * 0.25);
        assert!(raw > 0.0 && fitted_resonances.iter().all(|&v| v > 0.0), "{inputs}");
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        { let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: f.module, generation, armed: true,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id: f.module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: delay, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        bombs::start(&cx, &fit, &mut cycle).await.expect("bomb cycle");
        assert_eq!(loaded(&test_db, f.ship, FLAG_HI_SLOT_FIRST).await.ok().unwrap(), Some((ammo, 1)));
        assert_eq!(test_world.ball(1, f.ship).await.unwrap().health[0], 1.0);
        let ship = f.ship;
        // At the midpoint this bomb reaches the blast boundary at its nominal
        // deadline; the actor can sample it later on the next timer tick.
        test_world.with(1, move |s| s.set_ball_position(ship, intended_launcher_position, |_, _| true)).await.unwrap();
        assert!(test_world.after(1, 1500).await);
        let projectile = test_world.with(1, move |s| s.balls().find(|b| b.type_id == ammo && b.owner_id == f.character && b.id != f.ship).map(|b| b.id)).await.flatten().expect("flying bomb");
        let position = test_world.with(1, move |s| s.sample_position(projectile)).await.flatten().unwrap();
        let launcher_position = test_world.with(1, move |s| s.sample_position(ship)).await.flatten();
        eprintln!("bomb early flight: projectile_position={position:?}, launcher_position={launcher_position:?}, intended_launcher_position={intended_launcher_position:?}");
        assert!(position[2] < 0.0 && position[0].abs() < 1e-6 && position[1].abs() < 1e-6);
        assert!(radius >= velocity * crate::space::ms_to_seconds(delay) / 2.0, "{inputs}");
        let notified = tokio::time::timeout(std::time::Duration::from_millis(12000), async {
            loop {
                let (event, _) = rx.recv().await.expect("pilot notification channel closed");
                if event == "OnDamageMessage" { break; }
            }
        }).await;
        if notified.is_err() {
            let positions = test_world.with(1, move |s| (s.sample_position(projectile), s.sample_position(ship))).await;
            panic!("bomb damage notification timed out: {inputs}, positions={positions:?}");
        }
        let fraction = test_world.ball(1, f.ship).await.unwrap().health[0];
        assert!((fraction - (1.0 - raw / SHIELD)).abs() < 1e-9, "bomb must apply raw damage: {inputs}, health={fraction}");
        assert!(test_world.ball(1, projectile).await.is_none());
        fixture::admit_npc(&test_world, 1, fixture::ball(second_victim, hull, f.character, None, 0.0)).await;
        let second_position = intended_launcher_position.map(|coordinate| coordinate * 2.0);
        test_world.with(1, move |s| {
            s.set_ball_position(second_victim, second_position, |_, _| true);
        }).await.unwrap();
        let before_full = (test_world.ball(1, f.ship).await.unwrap().health, test_world.ball(1, second_victim).await.unwrap().health);
        let launch_position = test_world.with(1, move |s| s.sample_position(ship)).await.flatten().unwrap();
        bombs::start(&cx, &fit, &mut cycle).await.expect("second bomb cycle");
        test_world.with(1, move |s| { s.set_ball_position(ship, second_position, |_, _| true); }).await.unwrap();
        let replacement = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: replacement.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, replacement.clone());
        }
        assert!(test_world.after(1, 1500).await);
        let (second_projectile, launcher_position, victim_position) = test_world.with(1, move |s| {
            let projectile = s.balls().find(|b| b.type_id == ammo && b.owner_id == f.character && b.id != f.ship)
                .and_then(|b| s.sample_position(b.id));
            (projectile, s.sample_position(ship), s.sample_position(second_victim))
        }).await.unwrap();
        let projectile_position = second_projectile.expect("second bomb must still be flying");
        assert!(projectile_position[2] < launch_position[2], "second bomb has not advanced: {projectile_position:?}");
        assert_eq!(launcher_position, Some(second_position));
        assert_eq!(victim_position, Some(second_position));
        let predicted_center = [launch_position[0], launch_position[1], launch_position[2] - nominal_travel];
        assert!(crate::space::dist(predicted_center, second_position) < radius,
            "second detonation must contain both victims: {inputs}, center={predicted_center:?}, target={second_position:?}");
        let mut observed = Vec::new();
        let refusal = tokio::time::timeout(std::time::Duration::from_millis(12000), async {
            loop {
                let (event, args) = rx.recv().await.expect("pilot notification");
                if event == "OnRemoteMessage" && args.first() == Some(&marshal::Value::str(infinity_core::messages::MSG_CUSTOM_NOTIFY)) { break; }
                observed.push((event, args));
            }
        }).await;
        if refusal.is_err() {
            let state = test_world.with(1, move |s| {
                let projectile = s.balls().find(|b| b.type_id == ammo && b.owner_id == f.character && b.id != f.ship)
                    .and_then(|b| s.sample_position(b.id));
                (projectile, s.sample_position(ship), s.sample_position(second_victim),
                    s.combat_membership_generation(ship), s.combat_membership_generation(second_victim),
                    s.combat_phase(ship), s.combat_phase(second_victim))
            }).await;
            panic!("bomb capacity refusal timed out: {inputs}, positions and membership={state:?}, notifications={observed:?}");
        }
        assert!(!replacement.load(Ordering::Relaxed));
        assert!(combat::state().active.get(&f.module).is_some_and(|active| Arc::ptr_eq(&active.stop, &replacement)));
        assert_eq!((test_world.ball(1, f.ship).await.unwrap().health, test_world.ball(1, second_victim).await.unwrap().health), before_full);
    }).await;
    { let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    let _ = world.with(1, move |s| {
        let ids: Vec<_> = s.balls().filter(|b| b.type_id == ammo && b.owner_id == f.character && b.id != f.ship).map(|b| b.id).collect();
        for id in ids { s.destroy_ball(id); }
    }).await;
    world.remove_ball(1, f.ship).await;
    world.remove_ball(1, second_victim).await;
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(f.ship);
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn vorton_spends_one_charge_and_limits_arcs_around_primary() {
    use infinity_core::consts::{dogma::ATTR_DAMAGE_MULTIPLIER, dogma::ATTR_MAX_LOCKED_TARGETS, dogma::ATTR_MAX_RANGE, dogma::ATTR_MAX_TARGET_RANGE, dogma::ATTR_SCAN_RESOLUTION, CATEGORY_CHARGE, dogma::EFFECT_CHAIN_LIGHTNING};
    use crate::services::{combat::targeting_charges::{add_target_ctx, clear_locks_on, loaded, targets_of}, effects::vorton};
    use marshal::Value;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 100.0)
        && t.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).is_some_and(|v| v > 0.0)).expect("shielded hull").type_id;
    let gun = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE) && t.effects().iter().any(|e| e.effect_id == EFFECT_CHAIN_LIGHTNING)).expect("vorton weapon");
    let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
        .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| gun.attribute(a.attribute_id)).collect();
    let ammo = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)
        && t.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
        && combat::ATTR_DAMAGE.iter().any(|&a| t.attribute(a).is_some_and(|v| v > 0.0))).expect("vorton ammunition").type_id;
    let weapon = gun.type_id;
    let effect = registry::resolve(&sde, EFFECT_CHAIN_LIGHTNING).expect("vorton effect");
    assert!(effect.guid.is_some(), "vorton FX missing");
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, weapon, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let ids = std::array::from_fn::<_, 4, _>(|_| crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed));
    let result = tokio::spawn(async move {
        for (item, attr, value) in [(f.ship, ATTR_MAX_TARGET_RANGE, 100_000.0), (f.ship, ATTR_MAX_LOCKED_TARGETS, 4.0),
            (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0), (f.module, ATTR_MAX_RANGE, 100_000.0),
            (f.module, ATTR_DAMAGE_MULTIPLIER, 0.01), (f.module, 3036, 100.0), (f.module, 3037, 2.0)] {
            dogma::set_override(&test_db, item, attr, value).await.unwrap();
        }
        test_db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,2)", &[&f.ship,&FLAG_HI_SLOT_FIRST,&ammo]).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        for (id, x) in ids.into_iter().zip([1000.0, 1050.0, 1070.0, 1200.0]) {
            fixture::admit_npc(&test_world, 1, fixture::ball(id, hull, f.character, None, x)).await;
        }
        let (destiny, mut events) = tokio::sync::mpsc::unbounded_channel();
        test_world.observe(1, f.character, f.ship, destiny).await.unwrap();
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        add_target_ctx(&cx, ids[0], false).await.expect("lock request");
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while !targets_of(f.character).contains(&ids[0]) { assert!(test_world.after(1, 100).await); }
        }).await.expect("target lock");
        let fit = Fit::load(&test_db, sde.clone(), f.character, f.ship).await.ok().expect("load fit");
        let generation = Arc::new(AtomicBool::new(false));
        { let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: Some(ids[0]), repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: f.module, generation, armed: true,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id: f.module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        let cancelled = crate::services::effects::lifecycle::prepare_combat_source(&mut cycle, f.module, Some(weapon)).await.unwrap().unwrap();
        let cancelled_facts = cancelled.facts();
        drop(cancelled);
        let native_generation = cycle.owner.combat_generations[0].generation;
        let effect_id = cycle.effect.id;
        let replay = test_world.with(1, move |s| s.prepare_combat_source(f.ship, Some(f.module), Some(effect_id), Some(weapon),
            crate::lua::events::resources::FactOrigin::Natural, native_generation, cancelled_facts.sequence, 1, |_| true)).await.unwrap();
        assert!(matches!(replay, Err(crate::space::combat_facts::Refusal::Replay)));
        vorton::start(&cx, &fit, &mut cycle).await.expect("vorton cycle");
        let mut reports = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while reports.len() < 2 {
                let (event, args) = rx.recv().await.expect("pilot notification");
                if event == "OnDamageMessage" { reports.push(args); }
            }
        }).await.expect("vorton damage reports");
        assert_eq!(reports.len(), 2);
        assert_eq!(loaded(&test_db, f.ship, FLAG_HI_SLOT_FIRST).await.ok().unwrap(), Some((ammo, 1)));
        for (index, id) in ids.into_iter().enumerate() {
            let ball = test_world.ball(1, id).await.unwrap();
            let fraction = npc_health_fraction(&ball, 0).unwrap();
            if index < 2 { assert!(fraction < 1.0, "selected target must receive damage"); }
            else { assert_eq!(fraction, 1.0, "arc count and primary-centered range"); }
        }
        dogma::set_override(&test_db, f.module, ATTR_MAX_RANGE, 0.0).await.unwrap();
        dogma::set_override(&test_db, f.module, infinity_core::consts::dogma::ATTR_FALLOFF, 0.0).await.unwrap();
        let miss_fit = Fit::load(&test_db, sde, f.character, f.ship).await.ok().expect("load miss fit");
        let before_miss = test_world.ball(1, ids[0]).await.unwrap().health;
        vorton::start(&cx, &miss_fit, &mut cycle).await.expect("miss cycle");
        assert_eq!(test_world.ball(1, ids[0]).await.unwrap().health, before_miss);
        let mut misses = 0;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while misses < 2 {
                let (event, args) = rx.recv().await.expect("pilot notification");
                if event != "OnDamageMessage" { continue; }
                let Some(Value::Dict(fields)) = args.first() else { panic!("damage message shape") };
                assert!(fields.contains(&(Value::str("damage"), Value::Float(0.0))));
                assert!(fields.contains(&(Value::str("hitQuality"), Value::Int(0))));
                misses += 1;
            }
        }).await.expect("vorton miss reports");
        let args = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                for event in events.recv().await.expect("destiny observer") {
                    let Value::Tuple(envelope) = event else { continue };
                    let Some(Value::Tuple(call)) = envelope.get(1) else { continue };
                    if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                    let Some(Value::Tuple(args)) = call.get(1) else { continue };
                    if args.get(1) == Some(&Value::Int(f.module)) { return args.clone(); }
                }
            }
        }).await.expect("vorton FX");
        assert_eq!(args.len(), 14);
        assert_eq!(args[3], Value::Int(ids[0]));
        assert_eq!(args[13], Value::dict([("targets", Value::list([Value::Int(ids[1])]))]));
    }).await;
    { let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    clear_locks_on(&world, 1, f.ship, f.character);
    world.unobserve(f.character);
    for id in ids.into_iter().chain([f.ship]) { world.remove_ball(1, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn removed_bomb_releases_its_native_attempt_at_expiry() {
    use infinity_core::consts::{CATEGORY_CHARGE, dogma::ATTR_EXPLOSION_DELAY};
    use crate::services::effects::{bombs, lifecycle};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let launcher = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE) && ty.group_id == Some(862)).unwrap();
    let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
        .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| launcher.attribute(a.attribute_id)).collect();
    let ammo = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.group_id.is_some_and(|group| groups.contains(&f64::from(group)))
        && ty.attribute(ATTR_EXPLOSION_DELAY).is_some_and(|delay| delay >= 3000.0 && delay <= 12000.0)
        && ty.attribute(infinity_core::consts::dogma::ATTR_MAX_VELOCITY).is_some_and(|velocity| velocity > 0.0)
        && ty.attribute(infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE).is_some_and(|radius| radius > 0.0)).unwrap().type_id;
    let weapon = launcher.type_id;
    let effect = launcher.effects().iter().find_map(|entry| registry::resolve(&sde, entry.effect_id)).unwrap();
    let before = fixture::fingerprint(&db).await.unwrap();
    let f = fixture::create(&db, hull, weapon, FLAG_HI_SLOT_FIRST).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,1)",
            &[&f.ship, &FLAG_HI_SLOT_FIRST, &ammo]).await.unwrap();
        test_world.system(1).await.unwrap();
        test_world.with(1, |s| crate::space::snapshot::facts_test_init(s, 1, 4)).await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(f.character, tx.clone());
        fixture::admit(&test_db, &test_world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
        let fit = Fit::load(&test_db, sde, f.character, f.ship).await.unwrap();
        let delay = fit.value(crate::services::fit::Key::Charge(f.module), ATTR_EXPLOSION_DELAY);
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(f.module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(f.module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: f.module, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: f.module, generation, armed: true,
                combat_generations: vec![lifecycle::NativeCombatGeneration { module_id: f.module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: delay, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false };
        bombs::start(&cx, &fit, &mut cycle).await.unwrap();
        assert!(test_world.after(1, 1500).await);
        let projectile = test_world.with(1, move |s| s.balls().find(|b| b.type_id == ammo && b.owner_id == f.character && b.id != f.ship).map(|b| b.id)).await.flatten().unwrap();
        test_world.remove_ball(1, projectile).await;
        assert!(test_world.after(1, crate::space::seconds_to_ms(crate::space::ms_to_seconds(delay)) + 2000).await);
        let next = lifecycle::prepare_combat_source(&mut cycle, f.module, Some(weapon)).await.unwrap();
        assert!(next.is_some(), "expired bomb must release the only pending attempt slot");
        drop(next);
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&f.module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&f.module);
    }
    world.remove_ball(1, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(f.ship);
    drop(world);
    fixture::cleanup(&db, f).await.unwrap();
    assert_eq!(fixture::fingerprint(&db).await.unwrap(), before);
    flush.unwrap();
    result.unwrap();
}
