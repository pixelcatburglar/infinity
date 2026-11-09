//! Live slash override publication and resource reconciliation.
use super::*;
use infinity_core::{consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, dogma::{ATTR_CPU, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_OUTPUT, EFFECT_ONLINE}}};
use crate::services::effects::tests::fixture;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn live_override_publishes_without_projection_refresh_and_reconciles_offline() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_CPU_OUTPUT).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| i64::from(e.effect_id) == EFFECT_ONLINE)
        && !t.effects().iter().any(|e| sde.dogma.effect(e.effect_id).is_some_and(|effect|
            effect.modifiers().iter().any(|m| m.domain == "shipID" && m.modified == ATTR_CPU_OUTPUT))))
        .unwrap().type_id;
    let rows = fixture::create(&db, hull, module_type, FLAG_HI_SLOT_FIRST).await.unwrap();
    for (item, attr, value) in [(rows.ship, ATTR_CPU_OUTPUT, 1000.0), (rows.ship, ATTR_POWER_OUTPUT, 1_000_000.0),
        (rows.module, ATTR_CPU, 100.0), (rows.module, ATTR_POWER, 0.0)] {
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
        let module = rows.module;
        let before = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert_eq!(before.1, (1000.0, Some((true, false))));
        let attr = ATTR_CPU_OUTPUT.to_string();
        let reply = command(&mut client, &["ship".into(), attr.clone(), "1100".into()]).await.unwrap();
        assert_eq!(reply, messages::gm::gm_dogma_result(ship, &attr, ATTR_CPU_OUTPUT, 1000.0, 1100.0, Some((1000.0, 1100.0)), true));
        let after = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert_eq!(after, (before.0, (1100.0, Some((true, false)))));
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() { notices.push(event); }
        assert!(notices.iter().any(|(name, _)| name == "OnModuleAttributeChanges"));
        let reply = command(&mut client, &["ship".into(), attr.clone(), "0".into()]).await.unwrap();
        assert_eq!(reply, messages::gm::gm_dogma_result(ship, &attr, ATTR_CPU_OUTPUT, 1100.0, 0.0, Some((1100.0, 0.0)), true));
        let after_cascade = world.with(1, move |s| (s.projection_revision(ship), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert_eq!(after_cascade, (before.0, (0.0, Some((false, false)))));
        assert!(dao::online(&client.db, ship).await.unwrap().iter().all(|entry| entry.item_id != module));
        let mut notices = Vec::new();
        while let Ok(event) = rx.try_recv() { notices.push(event); }
        assert!(notices.iter().any(|(name, _)| name == "OnModuleAttributeChanges"));
        let (generation, revision) = world.with(1, move |s| (s.combat_membership_generation(ship).unwrap(), s.combat_fit_revision(ship).unwrap())).await.unwrap();
        let stale = Fit::load_in_space(&client.db, &world, 1, rows.character, ship).await.unwrap();
        let projection = stale.projection_revision();
        world.with(1, move |s| s.fitted_module_active(ship, module, true)).await.unwrap();
        let mutated = world.with(1, move |s| (s.combat_fit_revision(ship).unwrap(), crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap())).await.unwrap();
        assert!(mutated.0 > revision);
        assert_eq!(mutated.1, (0.0, Some((false, true))));
        assert!(!world.with(1, move |s| s.replace_fitted_cache(ship, generation, revision, stale, Key::Ship, projection)).await.unwrap());
        let cached = world.with(1, move |s| crate::space::combat::tests::cached_fitting(s, ship, module, ATTR_CPU_OUTPUT).unwrap()).await.unwrap();
        assert_eq!(cached, (0.0, Some((false, true))));
    }).await;
    cleanup_world.remove_ball(1, rows.ship).await;
    cleanup_world.unregister_pilot_generation(rows.character, None);
    cleanup_world.flush_positions().await.unwrap();
    crate::services::combat::forget_ship(rows.ship);
    fixture::cleanup(&db, rows).await.unwrap();
    result.unwrap();
}
