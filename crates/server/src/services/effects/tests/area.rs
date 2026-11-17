//! Smartbomb and bomb damage through real fitted effects and the system actor.
use super::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
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
    let mut f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: weapon, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let near = fixture::item(&db, hull, f.character, 1, 0, 1, true, false).await.unwrap();
    let outside = fixture::item(&db, hull, f.character, 1, 0, 1, true, false).await.unwrap();
    f.items.extend([near, outside]);
    let hub_id = crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed);
    let result = tokio::spawn(async move {
        dogma::set_override(&test_db, module_id, infinity_core::consts::dogma::ATTR_EMP_FIELD_RANGE, 100.0).await.unwrap();
        for attr in combat::ATTR_DAMAGE { dogma::set_override(&test_db, module_id, attr, if attr == ATTR_EM_DAMAGE { 1.0 } else { 0.0 }).await.unwrap(); }
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
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module_id, generation, armed: true,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
                source_membership: None, target_type: None, inputs: Default::default() };
        let source_before = test_world.ball(1, f.ship).await.unwrap().health;
        let hub_before = test_world.ball(1, hub_id).await.unwrap().health;
        cycle = fixture::capture_cycle(cycle).await;
        aoe::start(&cx, &mut cycle).await.expect("smartbomb cycle");
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
        assert!(matches!(aoe::start(&cx, &mut cycle).await, Err(crate::services::CallError::User { msg, .. })
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
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    for id in [f.ship, near, outside, hub_id] { world.remove_ball(1, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id, near, outside]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
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
    let effect = registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_BOMB_LAUNCHING).expect("bomb effect");
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: weapon, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
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
        let key = Key::Charge(module_id);
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
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module_id, generation, armed: true,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: delay, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
                source_membership: None, target_type: None, inputs: Default::default() };
        cycle = fixture::capture_cycle(cycle).await;
        bombs::start(&cx, &mut cycle).await.expect("bomb cycle");
        test_world.flush_positions().await.unwrap();
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
        cycle = fixture::capture_cycle(cycle).await;
        bombs::start(&cx, &mut cycle).await.expect("second bomb cycle");
        test_world.with(1, move |s| { s.set_ball_position(ship, second_position, |_, _| true); }).await.unwrap();
        let replacement = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: replacement.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, replacement.clone());
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
        assert!(combat::state().active.get(&module_id).is_some_and(|active| Arc::ptr_eq(&active.stop, &replacement)));
        assert_eq!((test_world.ball(1, f.ship).await.unwrap().health, test_world.ball(1, second_victim).await.unwrap().health), before_full);
    }).await;
    { let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
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
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
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
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: weapon, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let ids = std::array::from_fn::<_, 4, _>(|_| crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed));
    let result = tokio::spawn(async move {
        for (item, attr, value) in [(f.ship, ATTR_MAX_TARGET_RANGE, 100_000.0), (f.ship, ATTR_MAX_LOCKED_TARGETS, 4.0),
            (f.ship, ATTR_SCAN_RESOLUTION, 1_000_000.0), (module_id, ATTR_MAX_RANGE, 100_000.0),
            (module_id, ATTR_DAMAGE_MULTIPLIER, 0.01), (module_id, 3036, 100.0), (module_id, 3037, 2.0)] {
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
        let generation = Arc::new(AtomicBool::new(false));
        { let mut state = combat::state();
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: Some(ids[0]), repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module_id, generation, armed: true,
                combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
                source_membership: None, target_type: None, inputs: Default::default() };
        let cancelled = crate::services::effects::lifecycle::combat_source::prepare_combat_source(&mut cycle, module_id, Some(weapon)).await.unwrap().unwrap();
        let cancelled_facts = cancelled.facts();
        drop(cancelled);
        let native_generation = cycle.owner.combat_generations[0].generation;
        let effect_id = cycle.effect.id;
        let replay = test_world.with(1, move |s| s.prepare_combat_source(f.ship, Some(module_id), Some(effect_id), Some(weapon),
            crate::lua::events::resources::FactOrigin::Natural, native_generation, cancelled_facts.sequence, 1, |_| true)).await.unwrap();
        assert!(matches!(replay, Err(crate::space::combat_facts::Refusal::Replay)));
        cycle = fixture::capture_cycle(cycle).await;
        vorton::start(&cx, &mut cycle).await.expect("vorton cycle");
        let mut reports = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while reports.len() < 2 {
                let (event, args) = rx.recv().await.expect("pilot notification");
                if event == "OnDamageMessage" { reports.push(args); }
            }
        }).await.expect("vorton damage reports");
        assert_eq!(reports.len(), 2);
        test_world.flush_positions().await.unwrap();
        assert_eq!(loaded(&test_db, f.ship, FLAG_HI_SLOT_FIRST).await.ok().unwrap(), Some((ammo, 1)));
        for (index, id) in ids.into_iter().enumerate() {
            let ball = test_world.ball(1, id).await.unwrap();
            let fraction = npc_health_fraction(&ball, 0).unwrap();
            if index < 2 { assert!(fraction < 1.0, "selected target must receive damage"); }
            else { assert_eq!(fraction, 1.0, "arc count and primary-centered range"); }
        }
        for (attr, value) in [(infinity_core::consts::dogma::ATTR_AOE_CLOUD_SIZE, 111.0),
            (infinity_core::consts::dogma::ATTR_AOE_VELOCITY, 222.0),
            (infinity_core::consts::dogma::ATTR_AOE_DAMAGE_REDUCTION_FACTOR, 3.3)] {
            dogma::set_override(&test_db, module_id, attr, value).await.unwrap();
        }
        fixture::publish_fit(&test_db, &test_world, 1, f.character, f.ship).await;
        cycle = fixture::capture_cycle(cycle).await;
        let crate::services::effects::parameters::MemberPacket::Vorton(inputs) = &cycle.inputs.members[0].packet
            else { panic!("vorton member packet") };
        assert_eq!((inputs.aoe_cloud, inputs.aoe_velocity, inputs.drf), (111.0, 222.0, 3.3));
        let args = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let batch = match events.recv().await.expect("destiny observer") {
                    crate::space::SceneDelivery::Destiny(batch) => batch,
                    crate::space::SceneDelivery::Notify { event, .. } => { assert_eq!(event, "OnFleetStateChange"); continue; }
                };
                for event in batch {
                    let Value::Tuple(envelope) = event else { continue };
                    let Some(Value::Tuple(call)) = envelope.get(1) else { continue };
                    if call.first() != Some(&Value::str("OnSpecialFX")) { continue; }
                    let Some(Value::Tuple(args)) = call.get(1) else { continue };
                    if args.get(1) == Some(&Value::Int(module_id)) { return args.clone(); }
                }
            }
        }).await.expect("vorton FX");
        assert_eq!(args.len(), 14);
        assert_eq!(args[3], Value::Int(ids[0]));
        assert_eq!(args[13], Value::dict([("targets", Value::list([Value::Int(ids[1])]))]));
    }).await;
    { let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    clear_locks_on(&world, 1, f.ship, f.character);
    world.unobserve(f.character);
    for id in ids.into_iter().chain([f.ship]) { world.remove_ball(1, id).await; combat::forget_ship(id); }
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
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
    let effect = registry::resolve(&sde, infinity_core::consts::dogma::EFFECT_BOMB_LAUNCHING).expect("bomb effect");
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: weapon, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module_id = f.module();
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
        let delay = fit.value(crate::services::fit::Key::Charge(module_id), ATTR_EXPLOSION_DELAY);
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut state = combat::state();
            state.active.insert(module_id, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
            state.activation_generation.insert(module_id, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module_id, type_id: weapon, flag: FLAG_HI_SLOT_FIRST }], effect,
            target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module_id, generation, armed: true,
                combat_generations: vec![lifecycle::NativeCombatGeneration { module_id, generation: crate::space::targeting::next_generation(), sequence: 0 }],
                pending_combat: None, world: Arc::downgrade(&cx.world), system: cx.system_id },
            started: 0, charge_type: None, cycles: 0, duration_ms: delay, nominal_start: 0, other: None, repair_amount: 0.0, illegal: false,
                source_membership: None, target_type: None, inputs: Default::default() };
        cycle = fixture::capture_cycle(cycle).await;
        bombs::start(&cx, &mut cycle).await.unwrap();
        assert!(test_world.after(1, 1500).await);
        let projectile = test_world.with(1, move |s| s.balls().find(|b| b.type_id == ammo && b.owner_id == f.character && b.id != f.ship).map(|b| b.id)).await.flatten().unwrap();
        test_world.remove_ball(1, projectile).await;
        assert!(test_world.after(1, crate::space::seconds_to_ms(crate::space::ms_to_seconds(delay)) + 2000).await);
        let next = lifecycle::combat_source::prepare_combat_source(&mut cycle, module_id, Some(weapon)).await.unwrap();
        assert!(next.is_some(), "expired bomb must release the only pending attempt slot");
        drop(next);
    }).await;
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module_id) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module_id);
    }
    world.remove_ball(1, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(f.ship);
    drop(world);
    let rows = crate::services::tests::fixture::capture_fixture_rows(&db, &[crate::services::tests::fixture::Fixture { account: f.account, character: f.character }], &[f.ship, module_id]).await.unwrap();
    cleanup_ship(&db, None, f).await.unwrap();
    crate::services::tests::fixture::assert_fixture_removed(&db, &rows).await.unwrap();
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn smartbomb_pulse_destroys_in_range_missile_and_refusal_leaves_it_intact() {
    use infinity_core::consts::{CATEGORY_CHARGE, dogma::ATTR_STRUCTURE_HP};
    use crate::services::effects::{aoe::DamageGuard, space_source::PreparedCombatAttempt};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE)
        && t.attribute(ATTR_STRUCTURE_HP).is_some_and(|hp| hp > 0.0)).expect("damageable charge").type_id;
    let weapon = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_EMP_WAVE)).expect("smartbomb module").type_id;
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_world.system(1).await.unwrap();
        test_world.with(1, |s| crate::space::snapshot::facts_test_init(s, 8, 4)).await.unwrap();
        fixture::admit_npc(&test_world, 1, fixture::ball(1, hull, 1, None, 0.0)).await;
        let (tx, rx) = std::sync::mpsc::channel();
        test_world.with(1, move |s| s.launch_missile(fixture::ball(2, charge, 1, None, 50.0), 1, 1, 600_000.0,
            move |_, i| tx.send(i).unwrap())).await.unwrap();
        assert!(test_world.with(1, |s| s.projectile_damageable(2)).await.unwrap());
        let attempt = |sequence: u64| {
            let world = test_world.clone();
            async move {
                let facts = world.with(1, move |s| s.prepare_combat_source(1, None, None, Some(weapon),
                    crate::lua::events::resources::FactOrigin::Natural,
                    crate::space::targeting::next_generation(), sequence, 1, |_| true)).await.unwrap().unwrap();
                PreparedCombatAttempt::new(facts, Arc::downgrade(&world), 1)
            }
        };
        // A refused source admits nothing and the in-range missile stays in flight.
        let victims = test_world.with(1, |s| aoe::victims(s, 1, [0.0; 3], 1_000.0)).await.unwrap();
        aoe::apply_damage_guarded(&test_db, &test_world, 1, weapon, victims, aoe::DamageApplication::Fixed([1e9; 4]),
            Some(DamageGuard::Npc(1, 999, 0)), attempt(1).await).await.unwrap();
        assert!(test_world.ball(1, 2).await.is_some() && rx.try_recv().is_err());
        // An admitted pulse destroys it once through the shared terminal.
        let victims = test_world.with(1, |s| aoe::victims(s, 1, [0.0; 3], 1_000.0)).await.unwrap();
        aoe::apply_damage_guarded(&test_db, &test_world, 1, weapon, victims, aoe::DamageApplication::Fixed([1e9; 4]),
            None, attempt(2).await).await.unwrap();
        assert!(test_world.ball(1, 2).await.is_none() && rx.try_recv().unwrap().is_none());
        assert!(test_world.ball(1, 1).await.is_some()
            && test_world.with(1, |s| s.combat_membership_generation(2).is_none()).await.unwrap());
    }).await;
    world.remove_ball(1, 1).await;
    combat::forget_ship(1);
    let flush = world.flush_positions().await;
    drop(world);
    flush.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn defender_no_target_preserves_loaded_charge_and_permit() {
    use crate::services::effects::behaviors;
    use infinity_core::consts::{CATEGORY_CHARGE, dogma::{EFFECT_DEFENDER_MISSILE_LAUNCHING, EFFECT_USE_MISSILES}};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let launcher = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_MODULE) && ty.group_id == Some(512)
        && ty.effects().iter().any(|row| row.effect_id == EFFECT_USE_MISSILES)).unwrap().type_id;
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE) && ty.group_id == Some(88)
        && ty.effects().iter().any(|row| row.effect_id == EFFECT_DEFENDER_MISSILE_LAUNCHING)).unwrap().type_id;
    let f = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[ShipItemSpec { type_id: launcher, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] }]).await.unwrap();
    let module = f.module();
    db.execute("INSERT INTO game.module_charges (ship_id, flag_id, type_id, quantity) VALUES ($1,$2,$3,1)", &[&f.ship, &FLAG_HI_SLOT_FIRST, &charge]).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    world.system(1).await.unwrap();
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(f.character, tx.clone());
    fixture::admit(&db, &world, 1, fixture::ball(f.ship, hull, f.character, Some(f.character), 0.0)).await;
    fixture::publish_fit(&db, &world, 1, f.character, f.ship).await;
    let effect = registry::resolve(&sde, EFFECT_USE_MISSILES).unwrap();
    let generation = Arc::new(AtomicBool::new(false));
    combat::state().active.insert(module, ActiveModule { char_id: f.character, stop: generation.clone(), forced_inactive: false });
    combat::state().activation_generation.insert(module, generation.clone());
    let cx = Ctx { db: db.clone(), world: world.clone(), notify: tx, char_id: f.character, ship_id: f.ship, system_id: 1 };
    let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: launcher, flag: FLAG_HI_SLOT_FIRST }], effect,
        target: None, repeat: 1, stop: generation.clone(), owner: ActivationOwner { master: module, generation, armed: true, combat_generations: vec![crate::services::effects::lifecycle::NativeCombatGeneration { module_id: module, generation: crate::space::targeting::next_generation(), sequence: 0 }],
            pending_combat: None, world: Arc::downgrade(&world), system: 1 }, started: 0, charge_type: None, cycles: 0, duration_ms: 1000.0,
        nominal_start: 0, other: None, repair_amount: 0.0, illegal: false, source_membership: None, target_type: None, inputs: Default::default() };
    cycle = fixture::capture_cycle(cycle).await;
    assert!(matches!(behaviors::on_cycle_start(&cx, &mut cycle, false).await.unwrap(), crate::services::effects::lifecycle::CycleOutcome::Stop("no Defender target")));
    assert_eq!(crate::services::combat::targeting_charges::loaded(&db, f.ship, FLAG_HI_SLOT_FIRST).await.unwrap().map(|(_, quantity)| quantity), Some(1));
    assert!(world.with(1, |s| !s.loaded_outstanding()).await.unwrap());
    {
        let mut state = combat::state();
        if let Some(active) = state.active.remove(&module) { active.stop.store(true, Ordering::Relaxed); }
        state.activation_generation.remove(&module);
    }
    world.remove_ball(1, f.ship).await;
    world.unregister_pilot_generation(f.character, None);
    let flush = world.flush_positions().await;
    combat::forget_ship(f.ship);
    drop(world);
    cleanup_ship(&db, None, f).await.unwrap();
    flush.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn signature_application_scales_victims_by_effective_signature() {
    use crate::services::effects::space_source::PreparedCombatAttempt;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 10.0)
        && t.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).is_some_and(|v| v > 0.0)).expect("shielded hull");
    let hull = hull_type.type_id;
    // EM damage that strips 40% of the shield at full application, so no
    // victim saturates and the signature ratios stay observable.
    let em = 0.4 * hull_type.attribute(ATTR_SHIELD_CAPACITY).unwrap()
        / hull_type.attribute(ATTR_SHIELD_EM_DAMAGE_RESONANCE).unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        test_world.system(1).await.unwrap();
        test_world.with(1, |s| s.ensure_combat_facts()).await.unwrap();
        fixture::admit_npc(&test_world, 1, fixture::ball(1, hull, 1, None, 0.0)).await;
        // Three identical hulls in one batch; only the victims' effective
        // signatures differ, so dealt ratios isolate the fitted
        // `min(1, target_signature / weapon_signature)` factor.
        for (id, signature) in [(2, 200.0), (3, 400.0), (4, 800.0)] {
            let mut victim = fixture::ball(id, hull, 1, None, id as f64 + 48.0);
            victim.signature_radius = Some(signature);
            fixture::admit_npc(&test_world, 1, victim).await;
        }
        let loss = |s: &crate::space::System, id: i64| 1.0 - npc_health_fraction(s.ball(id).unwrap(), 0).unwrap_or(1.0);
        assert_eq!(test_world.with(1, move |s| [2, 3, 4].map(|id| loss(s, id))).await.unwrap(), [0.0, 0.0, 0.0]);
        let facts = test_world.with(1, move |s| s.prepare_combat_source(1, None, None, Some(hull),
            crate::lua::events::resources::FactOrigin::Natural,
            crate::space::targeting::next_generation(), 1, 1, |_| true)).await.unwrap().unwrap();
        let victims = test_world.with(1, |s| aoe::victims(s, 1, [0.0; 3], 1_000.0)).await.unwrap();
        aoe::apply_damage_guarded(&test_db, &test_world, 1, hull, victims,
            aoe::DamageApplication::Signature { raw: [em, 0.0, 0.0, 0.0], signature_radius: 400.0 },
            None, PreparedCombatAttempt::new(facts, Arc::downgrade(&test_world), 1)).await.unwrap();
        let after = test_world.with(1, move |s| [2, 3, 4].map(|id| loss(s, id))).await.unwrap();
        // sig 200/400 -> 0.5x; sig 400/400 -> 1.0x; sig 800/400 clamps to 1.0x.
        assert!(after[0] > 0.0 && (after[1] - 2.0 * after[0]).abs() < 1e-3 && (after[2] - after[1]).abs() < 1e-3,
            "shield loss {after:?}");
        // A non-positive weapon signature applies the full vector, and no
        // victim ever receives a source lockout.
        let facts = test_world.with(1, move |s| s.prepare_combat_source(1, None, None, Some(hull),
            crate::lua::events::resources::FactOrigin::Natural,
            crate::space::targeting::next_generation(), 2, 1, |_| true)).await.unwrap().unwrap();
        let victims = test_world.with(1, |s| aoe::victims(s, 1, [0.0; 3], 1_000.0)).await.unwrap();
        aoe::apply_damage_guarded(&test_db, &test_world, 1, hull, victims,
            aoe::DamageApplication::Signature { raw: [em, 0.0, 0.0, 0.0], signature_radius: 0.0 },
            None, PreparedCombatAttempt::new(facts, Arc::downgrade(&test_world), 1)).await.unwrap();
        let second = test_world.with(1, move |s| [2, 3, 4].map(|id| loss(s, id))).await.unwrap();
        assert!(second.iter().all(|v| *v > after[1]) , "full-damage second apply {second:?}");
        let unlocked = test_world.with(1, |s| [2, 3, 4].iter().all(|&id|
            s.super_weapon_immobile_until(id).is_none() && s.super_weapon_jump_cloak_until(id).is_none())).await.unwrap();
        assert!(unlocked);
    }).await;
    for id in 1..=4 { world.remove_ball(1, id).await; combat::forget_ship(id); }
    let flush = world.flush_positions().await;
    drop(world);
    flush.unwrap();
    result.unwrap();
}
