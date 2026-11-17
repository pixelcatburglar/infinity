//! Live slash override publication and resource reconciliation.
use super::*;
use infinity_core::{consts::{CATEGORY_DRONE, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, dogma::{ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_MAX_VELOCITY, ATTR_POWER, ATTR_POWER_OUTPUT, EFFECT_ONLINE}}};
use crate::services::effects::tests::fixture;
use crate::services::tests::fixture::{cleanup_ship, create_ship, ShipItemSpec, ShipLocation};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn live_override_publishes_without_projection_refresh_and_reconciles_offline() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_CPU_OUTPUT).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && crate::services::effects::fitting::rack(&sde.types, t.type_id).is_some_and(|r| r.2 == FLAG_HI_SLOT_FIRST)
        && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)
        && !t.effects().iter().any(|e| sde.dogma.effect(e.effect_id).is_some_and(|effect|
            effect.modifiers().iter().any(|m| m.domain == "shipID" && m.modified == ATTR_CPU_OUTPUT))))
        .unwrap().type_id;
    let mut rows = create_ship(&db, hull, ShipLocation::Docked { station: 1 }, &[
        ShipItemSpec { type_id: module_type, flag_id: FLAG_HI_SLOT_FIRST, quantity: 1, singleton: true, online: true, attributes: &[] },
    ]).await.unwrap();
    let module = rows.module();
    for (item, attr, value) in [(rows.ship, ATTR_CPU_OUTPUT, 1000.0), (rows.ship, ATTR_POWER_OUTPUT, 1_000_000.0),
        (module, ATTR_CPU, 100.0), (module, ATTR_POWER, 0.0)] {
        assert!(dao::set_override(&db, item, attr, value).await.unwrap());
    }
    let (mut client, _peer) = crate::services::tests::client(db.clone(), rows.character).await;
    client.session.apply(vec![("shipid", Value::Int(rows.ship)), ("solarsystemid", Value::Int(1))]);
    client.login.user_id = rows.account;
    client.login.role = infinity_core::consts::GM_ROLES;
    let world = client.world.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    client.notify_tx = tx.clone();
    world.register_pilot(rows.character, tx);
    let cleanup_world = world.clone();
    let result = tokio::spawn(async move {
        let mut skills: Vec<_> = crate::services::skills::requirements::requirements(&client.sde, module_type)
            .map(|(skill, _)| (skill, 5)).collect();
        for attr in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS {
            if let Some(skill) = client.sde.types.get(module_type).and_then(|ty| ty.attribute(attr)).filter(|v| *v > 0.0) {
                let skill = skill as i32;
                if !skills.iter().any(|&(id, _)| id == skill) { skills.push((skill, 5)); }
            }
        }
        crate::services::skills::gm::apply(&mut client, rows.character, crate::services::skills::gm::Change::Set(skills)).await.ok().expect("seed required module skills");
        fixture::admit(&client.db, &world, 1, fixture::ball(rows.ship, hull, rows.character, Some(rows.character), 0.0)).await;
        let ship = rows.ship;
        let before = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert_eq!(before.1, (1000.0, Some((true, false))));
        let attr = ATTR_CPU_OUTPUT.to_string();
        let reply = command(&mut client, &["ship".into(), attr.clone(), "1100".into()]).await.unwrap();
        assert_eq!(reply, messages::gm::gm_dogma_result(ship, &attr, ATTR_CPU_OUTPUT, 1000.0, 1100.0, Some((1000.0, 1100.0)), true, false));
        let after = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert_eq!(after, (before.0, (1100.0, Some((true, false)))));
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() { notices.push(event); }
        assert!(notices.iter().any(|(name, _)| name == "OnModuleAttributeChanges"));
        let reply = command(&mut client, &["ship".into(), attr.clone(), "0".into()]).await.unwrap();
        assert_eq!(reply, messages::gm::gm_dogma_result(ship, &attr, ATTR_CPU_OUTPUT, 1100.0, 0.0, Some((1100.0, 0.0)), true, false));
        let after_cascade = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert_eq!(after_cascade, (before.0, (0.0, Some((false, false)))));
        assert!(dao::online(&client.db, ship).await.unwrap().iter().all(|entry| entry.item_id != module));
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() { notices.push(event); }
        assert!(notices.iter().any(|(name, _)| name == "OnModuleAttributeChanges"));
        // Reset clears the committed override through the same field delta; the
        // offlined member stays offline, and a locked no-op publishes nothing.
        let reply = command(&mut client, &["ship".into(), attr.clone(), "reset".into()]).await.unwrap();
        let base_value = client.sde.types.get(hull).and_then(|t| t.attribute(ATTR_CPU_OUTPUT)).unwrap_or_default();
        assert_eq!(reply, messages::gm::gm_dogma_result(ship, &attr, ATTR_CPU_OUTPUT, 0.0, base_value, Some((0.0, base_value)), false, false));
        let restored = world.with(1, move |s| crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap()).await.unwrap();
        assert_eq!(restored, (base_value, Some((false, false))));
        let noop_revision = world.with(1, move |s| s.combat_fit_revision(ship)).await.unwrap();
        let reply = command(&mut client, &["ship".into(), attr.clone(), "reset".into()]).await.unwrap();
        assert_eq!(reply, messages::gm::gm_dogma_noop(ship, ATTR_CPU_OUTPUT));
        assert_eq!(world.with(1, move |s| s.combat_fit_revision(ship)).await.unwrap(), noop_revision, "a locked no-op must not publish");
        let revision = world.with(1, move |s| s.combat_fit_revision(ship).unwrap()).await.unwrap();
        world.with(1, move |s| s.fitted_module_active(ship, module, true)).await.unwrap();
        let mutated = world.with(1, move |s| (s.combat_fit_revision(ship).unwrap(), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert!(mutated.0 > revision);
        assert_eq!(mutated.1, (base_value, Some((false, true))));
        let cached = world.with(1, move |s| crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap()).await.unwrap();
        assert_eq!(cached, (base_value, Some((false, true))));
        // A persisted capacity-mode write from a foreign GM caller republishes
        // the brain to the owner's pilot channel, not the caller; a finite
        // set/reset clears the mode without touching live module runtime.
        client.session.apply(vec![("shipid", Value::Int(ship))]);
        crate::dao::characters::selection::set_ship(&client.db, rows.character, ship).await.unwrap();
        command(&mut client, &["ship".into(), attr.clone(), "1100".into()]).await.unwrap();
        crate::services::effects::online::set_online(&crate::services::identity::Identity::of_client(&client), &client.db, &world, &client.sde, client.notify_tx.clone(), client.login.user_id, ship, module, true, |notification| crate::services::effects::online::dispatch_notification(notification, &world, &client.notify_tx, client.session.int("charid"))).await.unwrap();
        // An owned drone admits once and takes only its own committed override;
        // the owner hull's domain is untouched and the real owner is notified.
        let drone_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_DRONE) && t.published == Some(true)).unwrap().type_id;
        let drone: i64 = client.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,1,0,1,true) RETURNING item_id", &[&drone_type, &rows.character]).await.unwrap().get("item_id");
        let dfit = Fit::drone(&client.db, sde.clone(), rows.character, drone).await.unwrap();
        let prepared = crate::space::drone::PreparedDrone {
            ball: crate::space::Ball { id: drone, type_id: drone_type, owner_id: rows.character, corp_id: 1, alliance_id: -1, char_id: None, signature_radius: None,
                mode: crate::space::MODE_STOP, radius: 1.0, mass: 0.0, max_vel: 0.0, agility: 1.0, speed_fraction: 0.0,
                pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
                goto: [0.0; 3], follow_id: 0, follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
                warp: crate::space::WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
                cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() },
            spec: crate::services::drones::worker::spec(&dfit, Key::Drone(drone), drone_type).ok().expect("drone spec"),
            condition: crate::services::combat::prepare_condition_with_fit(&client.db, drone, &dfit).await.ok().expect("drone condition"),
            fit: dfit, key: Key::Drone(drone), npc_ignored: false,
        };
        world.with(1, move |s| s.restore_drone(prepared, std::sync::Arc::new(|_, _| None))).await.expect("restore drone");
        let ship_revision = world.with(1, move |s| s.combat_fit_revision(ship)).await.unwrap();
        command(&mut client, &[drone.to_string(), "maxVelocity".into(), "321.0".into()]).await.unwrap();
        let seen = world.with(1, move |s| s.with_combat_fit(drone, move |fit, _, _| fit.value(Key::Drone(drone), ATTR_MAX_VELOCITY))).await.flatten();
        assert_eq!(seen, Some(321.0), "drone override did not reach the resident drone fit");
        assert_eq!(world.with(1, move |s| s.combat_fit_revision(ship)).await.unwrap(), ship_revision, "drone patch touched the owner hull");
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() { notices.push(event); }
        assert!(notices.iter().any(|(name, _)| name == "OnModuleAttributeChanges"), "owner must see the drone attribute notice");
        world.remove_ball(1, drone).await;
        crate::services::combat::forget_ship(drone);
        let base = crate::services::inventory::hold_storage::static_capacity(&client.sde, hull).unwrap();
        command(&mut client, &[ship.to_string(), "capacity".into(), base.to_string()]).await.unwrap();
        let condition = world.with(1, move |s| {
            assert!(s.set_condition_fraction_mask(ship, 1 << 1, 0.5));
            s.fitted_module_active(ship, module, true);
            s.fitted_module_overloaded(ship, module, true);
            s.combat_condition_snapshot(ship).unwrap().condition
        }).await.unwrap();
        let projection = world.with(1, move |s| s.projection_revision(ship)).await.unwrap();
        let foreign = crate::services::tests::fixture::create(&client.db).await.unwrap();
        let (foreign_tx, mut foreign_rx) = tokio::sync::mpsc::unbounded_channel();
        let owner_sender = std::mem::replace(&mut client.notify_tx, foreign_tx);
        let owner_account = client.login.user_id;
        client.login.user_id = foreign.account;
        client.session.apply(vec![("charid", Value::Int(foreign.character))]);
        let foreign_result = crate::tests::catch_scenario(async {
            for action in ["infinity", "reset"] {
                while rx.try_recv().is_ok() {}
                command(&mut client, &[ship.to_string(), "capacity".into(), action.into()]).await.unwrap();
                let live = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
                assert_eq!(live, (projection, (1100.0, Some((true, true)))));
                let after_condition = world.with(1, move |s| s.combat_condition_snapshot(ship).unwrap().condition).await.unwrap();
                assert_eq!(after_condition.now[1..], condition.now[1..]);
                assert_eq!((after_condition.stats.max, after_condition.shield_at), (condition.stats.max, condition.shield_at));
                assert_eq!(world.with(1, move |s| crate::space::combat::tests::cached_overloaded(s, ship, module)).await.unwrap(), Some(true));
                let mut brain = false;
                while let Ok((name, _)) = rx.try_recv() { brain |= name == crate::services::dogma::brain::UPDATED_EVENT; }
                assert!(brain, "owner must receive the brain update");
                assert!(foreign_rx.try_recv().is_err(), "foreign caller must not receive the owner's brain");
                let mode = dao::overrides(&client.db, ship).await.unwrap().into_iter()
                    .find(|r| r.attribute_id == infinity_core::consts::dogma::ATTR_CAPACITY);
                assert_eq!(mode.is_some_and(|row| row.cargo_unlimited), action == "infinity");
            }
            assert!(dao::online(&client.db, ship).await.unwrap().iter().any(|entry| entry.item_id == module));
        }).await;
        client.notify_tx = owner_sender;
        client.login.user_id = owner_account;
        client.session.apply(vec![("charid", Value::Int(rows.character))]);
        crate::services::tests::fixture::cleanup(&client.db, foreign).await.unwrap();
        foreign_result.expect("foreign GM mode scenario failed");
        // A dying ship still answers the brain bootstrap, from a cold read that leaves the record dying.
        world.with(1, move |s| {
            assert_eq!(s.combat_phase(ship), Some(crate::space::combat::CombatPhase::Live));
            let source = crate::services::combat::damage::DamageSource {
                force_origin: Some(crate::lua::events::resources::FactOrigin::Gm), ..Default::default()
            };
            assert!(s.apply_damage(ship, [0.0; 4], source, true).unwrap().unwrap().lethal);
            assert!(matches!(s.combat_phase(ship), Some(crate::space::combat::CombatPhase::Dying { .. })));
        }).await.unwrap();
        let brain = crate::services::dogma::brain::bootstrap(&client).await.unwrap();
        let Value::Tuple(fields) = brain else { panic!("brain tuple"); };
        assert_eq!(fields.len(), 4);
        assert!(matches!(fields[0], Value::Int(_)));
        assert!(fields[1..].iter().all(|field| matches!(field, Value::List(_))));
        assert!(matches!(world.with(1, move |s| s.combat_phase(ship)).await.unwrap(), Some(crate::space::combat::CombatPhase::Dying { .. })));
    }).await;
    cleanup_world.remove_ball(1, rows.ship).await;
    cleanup_world.with(1, move |s| {
        if let Some(crate::space::combat::CombatPhase::Dying { token }) = s.combat_phase(rows.ship) {
            let generation = s.combat_membership_generation(rows.ship).unwrap();
            assert!(s.finalize_dying(rows.ship, token, generation));
        }
    }).await.unwrap();
    cleanup_world.unregister_pilot_generation(rows.character, None);
    cleanup_world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(rows.ship);
    rows.items.extend(db.query("SELECT item_id FROM game.items WHERE owner_id = $1", &[&rows.character]).await.unwrap().iter().map(|row| row.get::<_, i64>("item_id")));
    cleanup_ship(&db, None, rows).await.unwrap();
    result.unwrap();
}
