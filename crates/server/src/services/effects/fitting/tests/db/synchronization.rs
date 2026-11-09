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
    use crate::db::outbox::{Effect, Outbox};
    use crate::services::{combat, effects::online::{self, OnlineChanged}, fit::Fit};
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_MODULE)).unwrap().type_id;
    let fixture = super::fixture::create(&db, ship_type, module_type).await.unwrap();
    let world = crate::space::World::new(db.clone(), sde.clone(), 1);
    crate::dao::items::dogma::set_override(&db, fixture.ship, ATTR_SHIELD_CAPACITY, 200.0).await.unwrap();
    crate::dao::items::dogma::set_override(&db, fixture.ship, ATTR_SHIELD_CHARGE, 150.0).await.unwrap();
    let fences = online::capture_online_fences(&world, &[fixture.owner]).await;
    let entries = db.transaction(
        |tx, outbox: &mut Outbox<OnlineChanged>| {
            let sde = sde.clone();
            let fences = fences.clone();
            Box::pin(async move {
                assert!(crate::dao::items::ship::lock_reload_ship(tx, fixture.ship).await.map_err(crate::services::db_err)?);
                let before = online::fitting::capture(tx, &sde, &fences, fixture.owner, fixture.ship).await?;
                crate::dao::items::dogma::set_override(tx, fixture.ship, ATTR_SHIELD_CAPACITY, 100.0).await.map_err(crate::services::db_err)?;
                let final_fit = Fit::load(tx, sde, fixture.owner, fixture.ship).await?;
                online::fitting::finish(tx, before, final_fit, outbox).await?;
                Ok::<(), crate::services::CallError>(())
            })
        },
        |(), outbox| async move { Ok(outbox.into_entries()) },
    ).await.unwrap();
    let changes = entries.into_iter().find_map(|entry| match entry {
        Effect::Operation(OnlineChanged::StoredFitting { owner, changes }) if owner == fixture.owner => Some(changes),
        _ => None,
    }).expect("stored fitting batch");
    let index = changes.iter().position(|change| matches!(change, Value::Tuple(row) if row.get(3) == Some(&Value::Int(i64::from(ATTR_SHIELD_CAPACITY))))).unwrap();
    let Value::Tuple(pair) = &changes[index + 1] else { panic!("shield pair") };
    assert_eq!(pair.get(3), Some(&Value::Int(i64::from(ATTR_SHIELD_CHARGE))));
    assert_eq!(pair.get(5), Some(&Value::Float(100.0)));
    let fit = Fit::load(&db, sde, fixture.owner, fixture.ship).await.unwrap();
    let reloaded = combat::prepare_condition_with_fit(&db, fixture.ship, &fit).await.unwrap().scalars();
    assert_eq!(reloaded.0, 100.0);
    super::fixture::cleanup(&db, &world, fixture).await.unwrap();
}
