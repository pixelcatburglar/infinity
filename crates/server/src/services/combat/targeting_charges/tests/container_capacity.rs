//! Reservation completion with physical and native source rows.
use super::*;
use crate::dao::items::{loading::{self, ItemQuery}, pending_reloads, transfers};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn external_batch_checks_full_facts_and_every_missing_row() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let (module_type, charge_type) = compatible_ammunition(&sde);
    let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(infinity_core::consts::CATEGORY_SHIP)).unwrap().type_id;
    let box_type = sde.types.iter().find(|t| t.published == Some(true) && t.group_id == Some(infinity_core::consts::GROUP_CARGO_CONTAINER)).unwrap().type_id;
    let station = sde.map.stations().next().unwrap().station_id;
    let flag = infinity_core::consts::FLAG_FITTING_FIRST;
    let fixture = crate::services::effects::tests::fixture::create(&db, ship_type, module_type, flag).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let world = World::new(test_db.clone(), sde.clone(), 1);
        let (notify, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let source = transfers::insert(&test_db, box_type, fixture.character, station, infinity_core::consts::FLAG_HANGAR, 1).await.unwrap();
        test_db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&source]).await.unwrap();
        let contents = transfers::insert(&test_db, charge_type, fixture.character, source, FLAG_CARGO, 7).await.unwrap();
        for variant in 0..7 {
            let native = pending_reloads::insert(&test_db, fixture.character, fixture.ship, fixture.module, flag, charge_type, 2, FLAG_CARGO, fixture.ship).await.unwrap();
            let physical = pending_reloads::insert(&test_db, fixture.character, fixture.ship, fixture.module, flag, charge_type, 3, FLAG_CARGO, source).await.unwrap();
            let static_row = pending_reloads::insert(&test_db, fixture.character, fixture.ship, fixture.module, flag, charge_type, 1, infinity_core::consts::FLAG_HANGAR, station).await.unwrap();
            let rows = pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap();
            let fact = rows.iter().find(|row| row.reload_id == physical).unwrap().clone();
            assert_eq!((fact.reload_id, fact.owner_id, fact.ship_id, fact.module_id, fact.flag_id, fact.type_id, fact.quantity, fact.source_flag, fact.source_location_id),
                (physical, fixture.character, fixture.ship, fixture.module, flag, charge_type, 3, FLAG_CARGO, source));
            let pending = PendingLoad { owner: fixture.character, reload_ids: vec![native, physical, static_row], external_sources: vec![source], external_facts: vec![fact], load: (fixture.module, flag, charge_type, 6, 0) };
            match variant {
                1 => { pending_reloads::take(&test_db, native).await.unwrap(); }
                2 => { pending_reloads::take(&test_db, physical).await.unwrap(); }
                3 => { pending_reloads::take(&test_db, static_row).await.unwrap(); }
                4 => { test_db.execute("UPDATE game.pending_reloads SET quantity=quantity+1 WHERE reload_id=$1", &[&physical]).await.unwrap(); }
                5 => { test_db.execute("UPDATE game.pending_reloads SET source_flag=$2 WHERE reload_id=$1", &[&physical, &infinity_core::consts::FLAG_HANGAR]).await.unwrap(); }
                6 => { test_db.execute("UPDATE game.pending_reloads SET source_location_id=$2 WHERE reload_id=$1", &[&physical, &fixture.ship]).await.unwrap(); }
                _ => {}
            }
            let before = pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap();
            let items_before = loading::load_items(&test_db, ItemQuery::Owner(fixture.character)).await.unwrap().into_iter()
                .map(|r| (r.item_id, r.type_id, r.owner_id, r.location_id, r.flag_id, r.quantity, r.singleton)).collect::<Vec<_>>();
            let outcome = apply_loads(&test_db, &world, &notify, fixture.ship, vec![pending]).await;
            if variant == 0 {
                outcome.unwrap();
                assert!(pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap().is_empty());
                assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), Some((charge_type, 6)));
                let (name, args) = rx.try_recv().unwrap();
                assert_eq!(name, "OnGodmaPrimeItem");
                super::charge_sync::assert_prime(&args, &sde, fixture.ship, flag, charge_type, 6);
                let (name, _) = rx.try_recv().unwrap();
                assert_eq!(name, "OnModuleAttributeChanges");
                crate::dao::items::combat::delete_charge(&test_db, fixture.ship, flag).await.unwrap();
            } else {
                assert!(outcome.is_err());
                assert!(pending_reloads::for_ship(&test_db, fixture.ship).await.unwrap() == before);
                assert_eq!(loaded(&test_db, fixture.ship, flag).await.unwrap(), None);
                assert!(rx.try_recv().is_err());
                for row in before { pending_reloads::take(&test_db, row.reload_id).await.unwrap(); }
            }
            let items_after = loading::load_items(&test_db, ItemQuery::Owner(fixture.character)).await.unwrap().into_iter()
                .map(|r| (r.item_id, r.type_id, r.owner_id, r.location_id, r.flag_id, r.quantity, r.singleton)).collect::<Vec<_>>();
            assert_eq!(items_after, items_before);
            assert_eq!(loading::load_items(&test_db, ItemQuery::Id(contents)).await.unwrap()[0].quantity, 7);
            assert!(rx.try_recv().is_err());
        }
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id=$1", &[&fixture.ship]).await.unwrap();
    crate::services::effects::tests::fixture::cleanup(&db, fixture).await.unwrap();
    result.unwrap();
}
