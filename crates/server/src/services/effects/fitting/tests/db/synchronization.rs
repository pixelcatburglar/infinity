//! Fitting publication checks against the disposable database fixture.
//! Inventory and attribute events are observed after the real movement transaction.
mod trashing;
mod gm;
mod fixture;
mod movement;
mod online;
mod stats;

use marshal::Value;
use tokio::sync::mpsc::UnboundedReceiver;

pub(super) fn installed_module_follows_inventory(events: &mut UnboundedReceiver<(String, Vec<Value>)>, owner: i64, module: i64) {
    let mut item_seen = false;
    let mut module_seen = false;
    while let Ok((event, args)) = events.try_recv() {
        if event == "OnItemChange" { item_seen = true; }
        if event != "OnModuleAttributeChanges" { continue; }
        let [Value::List(changes)] = args.as_slice() else { continue };
        for change in changes {
            let Value::Tuple(row) = change else { continue };
            if row.get(1) == Some(&Value::Int(owner)) && row.get(2) == Some(&Value::Int(module)) {
                assert!(item_seen, "fitted module attributes preceded the inventory row");
                module_seen = true;
            }
        }
    }
    assert!(item_seen, "missing inventory change");
    assert!(module_seen, "missing fitted module attributes");
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn stored_shield_rebase_is_checkpointed_and_notified() {
    use std::sync::Arc;
    use infinity_core::consts::{CATEGORY_MODULE, CATEGORY_SHIP, dogma::{ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE}};
    use crate::db::outbox::Outbox;
    use crate::services::{combat, effects::online::{self, OnlineChanged}, fit::Fit};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let fixture = crate::services::tests::fixture::create_ship(&db, ship_type, crate::services::tests::fixture::ShipLocation::Docked { station: 1 }, &[
        crate::services::tests::fixture::ShipItemSpec { type_id: module_type, flag_id: infinity_core::consts::FLAG_CARGO, quantity: 1, singleton: true, online: false, attributes: &[] },
        crate::services::tests::fixture::ShipItemSpec { type_id: module_type, flag_id: infinity_core::consts::FLAG_CARGO, quantity: 1, singleton: true, online: false, attributes: &[] },
    ]).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    let result = crate::tests::catch_scenario(Box::pin(async {
        let (mut client, _peer) = crate::services::tests::client(db.clone(), fixture.character).await;
        client.world = world.clone();
        client.session.apply(vec![("shipid", Value::Int(fixture.ship))]);
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender;
        crate::dao::items::dogma::set_override(&db, fixture.ship, ATTR_SHIELD_CAPACITY, 200.0).await.unwrap();
        crate::dao::items::dogma::set_override(&db, fixture.ship, ATTR_SHIELD_CHARGE, 150.0).await.unwrap();
        let fences = online::capture_online_fences(&world, &[fixture.character]).await;
        let version = crate::services::dogma::brain::bootstrap(&client).await.unwrap().items().unwrap()[0].as_int().unwrap();
        for step in 0..3 {
            if step == 1 {
                crate::dao::characters::selection::set_ship(&db, fixture.character, fixture.ship).await.unwrap();
            }
            let (owner, ship) = (fixture.character, fixture.ship);
            let result: Result<(), crate::services::CallError> = db.transaction(
                |tx, outbox: &mut Outbox<OnlineChanged>| {
                    let sde = sde.clone();
                    let fences = fences.clone();
                    Box::pin(async move {
                        assert!(crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(crate::services::db_err)?);
                        let before = online::fitting::capture(tx, &sde, &fences, owner, ship).await?;
                        if step == 0 {
                            crate::dao::items::dogma::set_override(tx, ship, ATTR_SHIELD_CAPACITY, 100.0).await.map_err(crate::services::db_err)?;
                            crate::dao::items::dogma::set_cargo_unlimited(tx, ship, 400.0).await.map_err(crate::services::db_err)?;
                        } else {
                            crate::dao::items::dogma::set_override(tx, ship, infinity_core::consts::dogma::ATTR_CAPACITY, 400.0).await.map_err(crate::services::db_err)?;
                        }
                        let final_fit = Fit::load(tx, sde, owner, ship).await?;
                        online::fitting::finish(tx, before, final_fit, outbox).await?;
                        if step == 1 { return Err(crate::services::db_err(anyhow::anyhow!("forced rollback"))); }
                        Ok(())
                    })
                },
                |(), outbox| {
                    let result = online::dispatch_changed((), outbox, &client);
                    async move { result }
                },
            ).await;
            if step == 1 {
                assert!(matches!(result, Err(crate::services::CallError::Internal(error)) if error.to_string() == "forced rollback"));
            } else {
                result.unwrap();
            }
            let mut notices = Vec::new();
            while let Ok(event) = events.try_recv() { notices.push(event); }
            if step == 0 {
                let changes = notices.iter().find(|(name, _)| name == "OnModuleAttributeChanges").expect("shield notification");
                let Value::List(changes) = &changes.1[0] else { panic!("attribute changes list") };
                let index = changes.iter().position(|change| matches!(change, Value::Tuple(row) if row.get(3) == Some(&Value::Int(i64::from(ATTR_SHIELD_CAPACITY))))).unwrap();
                let Value::Tuple(pair) = &changes[index + 1] else { panic!("shield pair") };
                assert_eq!(pair.get(3), Some(&Value::Int(i64::from(ATTR_SHIELD_CHARGE))));
                assert_eq!(pair.get(5), Some(&Value::Float(100.0)));
            }
            assert_eq!(notices.iter().any(|(name, _)| name == crate::services::dogma::brain::UPDATED_EVENT), step == 2);
            assert_eq!(crate::services::dogma::brain::bootstrap(&client).await.unwrap().items().unwrap()[0].as_int().unwrap(), version + i64::from(step == 2));
            let state = crate::dao::items::dogma::hull_override(&db, fixture.ship, infinity_core::consts::dogma::ATTR_CAPACITY).await.unwrap().unwrap().state.unwrap();
            assert_eq!(state, crate::dao::items::dogma::OverrideState { value: 400.0, cargo_unlimited: step != 2 });
            if step == 1 { assert!(notices.is_empty(), "rollback must not dispatch"); }
        }
        let fit = Fit::load(&db, sde.clone(), fixture.character, fixture.ship).await.unwrap();
        let reloaded = combat::prepare_condition_with_fit(&db, fixture.ship, &fit).await.unwrap().scalars();
        assert_eq!(reloaded.0, 100.0);
    })).await;
    world.unobserve(fixture.character);
    crate::services::tests::fixture::cleanup_ship(&db, Some(world.as_ref()), fixture).await.unwrap();
    result.expect("stored fitting publication failed");
}
