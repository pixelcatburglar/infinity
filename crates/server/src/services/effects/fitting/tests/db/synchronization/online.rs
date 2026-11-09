//! Online fitting transactions with real SDE resource modifiers.
use infinity_core::{consts::dogma::{ATTR_CPU, ATTR_CPU_LOAD, ATTR_CPU_OUTPUT, ATTR_POWER, ATTR_POWER_LOAD, ATTR_POWER_OUTPUT}};
use crate::services::fit::Key;
use super::fixture::{self, MovementFixture};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn online_resource_modifier_changes_once_and_refusal_rolls_back() {
    let f = MovementFixture::create_online_resource().await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let result = tokio::spawn(async move {
        let mut f = f;
        let ship = rows.ship;
        let module = rows.modules[0];
        assert!(f.requires_online);
        assert_eq!(f.move_to(ship, ship, f.slot).await.ok().flatten(), Some(module));
        f.drain();
        let before = f.value(ship, ATTR_CPU_OUTPUT).await;
        f.set_online(ship, module, true).await.unwrap();
        let online = f.value(ship, ATTR_CPU_OUTPUT).await;
        assert!(online > before, "real online SDE modifier must increase CPU output");
        assert_eq!(fixture::attribute_rows(&f.drain(), rows.owner, ship, ATTR_CPU_OUTPUT), vec![(before, online)]);
        f.set_online(ship, module, true).await.unwrap();
        assert!(fixture::attribute_rows(&f.drain(), rows.owner, ship, ATTR_CPU_OUTPUT).is_empty());
        f.set_online(ship, module, false).await.unwrap();
        let offline = f.value(ship, ATTR_CPU_OUTPUT).await;
        assert_eq!(offline, before);
        assert_eq!(fixture::attribute_rows(&f.drain(), rows.owner, ship, ATTR_CPU_OUTPUT), vec![(online, offline)]);
        f.set_online(ship, module, false).await.unwrap();
        assert!(fixture::attribute_rows(&f.drain(), rows.owner, ship, ATTR_CPU_OUTPUT).is_empty());
        let snapshot = super::super::fixture::fingerprint(&f.client.db).await.unwrap();
        assert!(f.set_online(ship, rows.modules[1], true).await.is_err());
        assert_eq!(super::super::fixture::fingerprint(&f.client.db).await.unwrap(), snapshot);
        assert!(f.drain().is_empty());
    }).await;
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated initialized database with SDE"]
async fn offline_resource_source_publishes_only_final_legal_cascade() {
    let f = MovementFixture::create_online_resource().await;
    let db = f.client.db.clone();
    let world = f.client.world.clone();
    let rows = f.rows;
    let result = tokio::spawn(async move {
        let mut f = f;
        let ship = rows.ship;
        let [source, survivor] = rows.modules;
        for module in [source, survivor] {
            for attr in [ATTR_CPU, ATTR_POWER] {
                assert!(crate::dao::items::dogma::set_override(&f.client.db, module, attr, 0.0).await.unwrap());
            }
        }
        assert_eq!(f.move_module(ship, ship, f.slot, source).await.ok().flatten(), Some(source));
        assert_eq!(f.move_module(ship, ship, f.slot + 1, survivor).await.ok().flatten(), Some(survivor));
        f.set_online(ship, survivor, true).await.unwrap();
        let survivor_capacity = f.value(ship, ATTR_CPU_OUTPUT).await;
        f.set_online(ship, source, true).await.unwrap();
        let before = f.value(ship, ATTR_CPU_OUTPUT).await;
        assert!(before > survivor_capacity);
        assert!(crate::dao::items::dogma::set_override(&f.client.db, survivor, ATTR_CPU, (before + survivor_capacity) / 2.0).await.unwrap());
        let fit = f.fit(ship).await;
        assert!(fit.value(Key::Ship, ATTR_CPU_LOAD) > survivor_capacity);
        let before_load = fit.value(Key::Ship, ATTR_CPU_LOAD);
        assert!(fit.value(Key::Ship, ATTR_CPU_LOAD) <= before);
        assert!(fit.value(Key::Ship, ATTR_POWER_LOAD) <= fit.value(Key::Ship, ATTR_POWER_OUTPUT));
        f.drain();
        f.set_online(ship, source, false).await.unwrap();
        let online = crate::dao::items::dogma::online(&f.client.db, ship).await.unwrap();
        assert!(online.iter().all(|entry| entry.item_id != source && entry.item_id != survivor));
        let fit = f.fit(ship).await;
        let final_cpu = fit.value(Key::Ship, ATTR_CPU_OUTPUT);
        assert!(final_cpu < survivor_capacity);
        let final_load = fit.value(Key::Ship, ATTR_CPU_LOAD);
        assert!(fit.value(Key::Ship, ATTR_CPU_LOAD) <= final_cpu);
        assert!(fit.value(Key::Ship, ATTR_POWER_LOAD) <= fit.value(Key::Ship, ATTR_POWER_OUTPUT));
        let events = f.drain();
        assert_eq!(fixture::attribute_rows(&events, rows.owner, ship, ATTR_CPU_OUTPUT), vec![(before, final_cpu)]);
        assert_eq!(fixture::attribute_rows(&events, rows.owner, ship, ATTR_CPU_LOAD), vec![(before_load, final_load)]);
    }).await;
    super::super::fixture::cleanup(&db, &world, rows).await.unwrap();
    result.unwrap();
}
