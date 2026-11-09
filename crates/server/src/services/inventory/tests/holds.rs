use super::*;
use std::sync::Arc;
use super::super::destination::{Custody, Destination};
use super::super::hold_storage::Replaced;

async fn move_test(
    tx: &(impl crate::db::executor::Executor + ?Sized), sde: &Arc<Sde>, outbox: &mut Outbox,
    item: &crate::dao::items::loading::Item, destination: &Destination, take: Option<i64>, custody: Custody,
) -> Result<Option<i64>, CallError> {
    use crate::dao::items::{deliveries, parent_locks::{LockRequest, LockStrength}};
    use super::super::destination::{lock_items, parent_requests};
    let mut parents = deliveries::ancestry(tx, item.location_id).await.map_err(db_err)?;
    parents.extend(deliveries::ancestry(tx, destination.location).await.map_err(db_err)?);
    let mut requests = parent_requests(sde, parents.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?;
    requests.push(LockRequest { item_id: item.item_id, strength: LockStrength::Update });
    let locks = lock_items(tx, sde, &requests, &[]).await?;
    move_core(tx, sde, outbox, item, destination, take, custody, &locks, None, None, item.owner_id).await
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn hold_refusal_leaves_source_unchanged() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let cargo_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::transfers::insert(&tx, ship_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.unwrap();
    let source = crate::dao::items::transfers::insert(&tx, cargo_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let item = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let additions = [super::super::hold_storage::Addition { type_id: cargo_type, singleton: false, quantity: 1 }];
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &additions, Replaced::default(), Some(0.0)).await.is_err());
    let mut outbox: Outbox = Outbox::default();
    assert!(move_test(&tx, &sde, &mut outbox, &item, &Destination::owned(1, ship, infinity_core::consts::FLAG_DRONE_BAY), None, Custody::Preserve).await.is_err());
    assert!(outbox.into_entries().is_empty());
    let after = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((after.location_id, after.flag_id, after.quantity), (item.location_id, item.flag_id, item.quantity));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn capacity_revalidates_locked_hull_owner_and_assembly() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.fit_attribute(ATTR_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let cargo_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::creation::rookie_ship(&tx, ship_type, 1, 1, FLAG_HANGAR).await.unwrap();
    let _cargo = crate::dao::items::transfers::insert(&tx, cargo_type, 1, ship, FLAG_CARGO, 1).await.unwrap();
    let (cap, used, _) = super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.unwrap().unwrap();
    assert!(cap > 0.0 && used > 0.0);
    let refuse = |result| assert!(matches!(result, Err(CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_CUSTOM_NOTIFY));
    refuse(super::super::hold_storage::capacity(&tx, &sde, 2, ship, FLAG_CARGO).await);
    let item = load_items(&tx, &sde, ItemQuery::Id(ship)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&tx, &item, item.location_id, item.flag_id, item.owner_id, false).await.unwrap());
    refuse(super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await);
    let item = load_items(&tx, &sde, ItemQuery::Id(ship)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::inventory::move_item(&tx, &item, item.location_id, item.flag_id, item.owner_id, true).await.unwrap());
    let item = load_items(&tx, &sde, ItemQuery::Id(ship)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::quantities::apply(&tx, &[(ship, crate::dao::items::quantities::QuantityChange::new(&item, Some(2)))].into_iter().collect()).await.unwrap());
    refuse(super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await);
    let item = load_items(&tx, &sde, ItemQuery::Id(ship)).await.unwrap().pop().unwrap();
    assert!(crate::dao::items::quantities::apply(&tx, &[(ship, crate::dao::items::quantities::QuantityChange::new(&item, Some(1)))].into_iter().collect()).await.unwrap());
    assert_eq!(super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.unwrap().map(|(cap, used, _)| (cap, used)), Some((cap, used)));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn docked_assembled_ship_round_trip_preserves_contents_and_flag_capacity() {
    use infinity_core::consts::FLAG_SHIP_HANGAR;
    use infinity_core::consts::dogma::ATTR_SHIP_MAINTENANCE_BAY_CAPACITY;
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let carrier = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v > 0.0)).unwrap();
    let child = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.volume.is_some_and(|v| v > 0.0 && v <= carrier.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).unwrap())
        && super::super::holds::accepts(&sde, FLAG_SHIP_HANGAR, t.type_id)).unwrap();
    let cargo_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::transfers::insert(&tx, carrier.type_id, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let stored = crate::dao::items::transfers::insert(&tx, child.type_id, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let cargo = crate::dao::items::transfers::insert(&tx, cargo_type, 1, stored, FLAG_CARGO, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[ship, stored][..]]).await.unwrap();
    let (cap, used, _) = super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_SHIP_HANGAR).await.unwrap().unwrap();
    assert!(cap > 0.0);
    assert_eq!(used, 0.0);
    let fit = crate::services::fit::Fit::load_unpiloted(&tx, sde.clone(), ship).await.unwrap();
    assert_eq!(cap, super::super::holds::effective_capacity(&fit, FLAG_SHIP_HANGAR));
    let mut outbox: Outbox = Outbox::default();
    let item = load_items(&tx, &sde, ItemQuery::Id(stored)).await.unwrap().pop().unwrap();
    move_test(&tx, &sde, &mut outbox, &item, &Destination::owned(1, ship, FLAG_SHIP_HANGAR), None, Custody::Preserve).await.unwrap();
    let (_, bay_used, _) = super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_SHIP_HANGAR).await.unwrap().unwrap();
    let (_, cargo_used, _) = super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.unwrap().unwrap();
    assert!(bay_used > 0.0);
    assert_eq!(cargo_used, 0.0);
    let item = load_items(&tx, &sde, ItemQuery::Id(stored)).await.unwrap().pop().unwrap();
    move_test(&tx, &sde, &mut outbox, &item, &Destination::owned(1, 1, FLAG_HANGAR), None, Custody::Preserve).await.unwrap();
    let content = load_items(&tx, &sde, ItemQuery::Id(cargo)).await.unwrap().pop().unwrap();
    assert_eq!((content.location_id, content.flag_id), (stored, FLAG_CARGO));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn reload_reservations_occupy_only_their_source_hold() {
    use infinity_core::consts::FLAG_SPECIALIZED_AMMO_HOLD;
    use crate::dao::items::pending_reloads;
    use super::super::hold_storage::{self, Addition};
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let charge = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let unit = crate::services::item_volume::item_volume(&sde, charge, false, 1, None).unwrap();
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::transfers::insert(&tx, ship_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.unwrap();
    let source = crate::dao::items::transfers::insert(&tx, charge, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let cargo = pending_reloads::insert(&tx, 1, ship, 1, 1, charge, 1, FLAG_CARGO, ship).await.unwrap();
    let ammo = pending_reloads::insert(&tx, 1, ship, 1, 1, charge, 1, FLAG_SPECIALIZED_AMMO_HOLD, ship).await.unwrap();
    let (_, cargo_used, _) = hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.unwrap().unwrap();
    assert_eq!(cargo_used, unit);
    assert_eq!(hold_storage::room(&tx, &sde, 1, ship, FLAG_SPECIALIZED_AMMO_HOLD, unit).await.unwrap().used, unit);
    let addition = [Addition { type_id: charge, singleton: false, quantity: 1 }];
    assert!(hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &addition, Replaced::default(), Some(unit)).await.is_err());
    let item = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((item.location_id, item.flag_id, item.quantity), (1, FLAG_HANGAR, 1));
    assert_eq!(pending_reloads::for_ship(&tx, ship).await.unwrap().len(), 2);
    pending_reloads::take(&tx, cargo).await.unwrap();
    assert_eq!(hold_storage::room(&tx, &sde, 1, ship, FLAG_CARGO, unit).await.unwrap().used, 0.0);
    assert_eq!(pending_reloads::take(&tx, ammo).await.unwrap().unwrap().source_flag, FLAG_SPECIALIZED_AMMO_HOLD);
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn raw_contents_packaged_hulls_and_same_bay_moves_are_checked() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP) && t.fit_attribute(ATTR_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let cargo_type = sde.types.iter().find(|t| t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.volume.is_some_and(|v| v > 0.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::transfers::insert(&tx, ship_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let source = crate::dao::items::transfers::insert(&tx, cargo_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let addition = [super::super::hold_storage::Addition { type_id: cargo_type, singleton: false, quantity: 1 }];
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &addition, Replaced::default(), Some(1000.0)).await.is_err());
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.unwrap();
    let unknown_type = i32::MAX;
    assert!(sde.types.get(unknown_type).is_none());
    let unknown = crate::dao::items::transfers::insert(&tx, unknown_type, 1, ship, FLAG_CARGO, 1).await.unwrap();
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &addition, Replaced::default(), Some(1000.0)).await.is_err());
    assert!(super::super::hold_storage::room(&tx, &sde, 1, ship, FLAG_CARGO, 1000.0).await.is_err());
    assert!(super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.is_err());
    let item = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    let mut refused: Outbox = Outbox::default();
    assert!(move_test(&tx, &sde, &mut refused, &item, &Destination::owned(1, ship, FLAG_CARGO), None, Custody::Preserve).await.is_err());
    assert!(refused.into_entries().is_empty());
    let invalid = super::super::hold_storage::Room { capacity: 1.0, used: f64::NAN };
    assert!(invalid.units(&sde, FLAG_CARGO, cargo_type, false, 1).is_err());
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, unknown, FLAG_CARGO, &addition, Replaced::default(), Some(1000.0)).await.is_err());
    tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&unknown]).await.unwrap();
    let item = load_items(&tx, &sde, ItemQuery::Id(source)).await.unwrap().pop().unwrap();
    assert_eq!((item.location_id, item.flag_id, item.quantity), (1, FLAG_HANGAR, 1));
    let unit = crate::services::item_volume::item_volume(&sde, cargo_type, false, 1, None).unwrap();
    assert!(crate::dao::items::dogma::set_override(&tx, ship, ATTR_CAPACITY, unit * 0.9).await.unwrap());
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &addition, Replaced::default(), None).await.is_err());
    let candidates: Vec<_> = sde.types.iter()
        .filter(|t| t.category_id == Some(infinity_core::consts::CATEGORY_MODULE) && t.published == Some(true)
            && t.modifiers(&sde.dogma, ATTR_CAPACITY).any(|(_, modifier)| modifier.domain == "shipID"))
        .map(|t| t.type_id).collect();
    let mut boosted = None;
    for type_id in candidates {
        let module = crate::dao::items::transfers::insert(&tx, type_id, 1, ship, FLAG_FITTING_FIRST, 1).await.unwrap();
        tx.execute("UPDATE game.items SET singleton = true, online = true WHERE item_id = $1", &[&module]).await.unwrap();
        let (capacity, _, _) = super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.unwrap().unwrap();
        if capacity >= unit {
            boosted = Some(module);
            break;
        }
        tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&module]).await.unwrap();
    }
    let module = boosted.expect("published cargo-capacity module with an applicable SDE modifier");
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &addition, Replaced::default(), None).await.is_ok());
    tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&module]).await.unwrap();
    assert!(crate::dao::items::dogma::set_override(&tx, ship, ATTR_CAPACITY, unit * 4.0).await.unwrap());
    let (effective, _, _) = super::super::hold_storage::capacity(&tx, &sde, 1, ship, FLAG_CARGO).await.unwrap().unwrap();
    assert_eq!(effective, unit * 4.0);
    let stack = crate::dao::items::transfers::insert(&tx, cargo_type, 1, ship, FLAG_CARGO, 4).await.unwrap();
    let stack_item = load_items(&tx, &sde, ItemQuery::Id(stack)).await.unwrap().pop().unwrap();
    let mut outbox: Outbox = Outbox::default();
    move_test(&tx, &sde, &mut outbox, &stack_item, &Destination::owned(1, ship, FLAG_CARGO), Some(1), Custody::Preserve).await.unwrap();
    let items = crate::dao::items::loading::load_items(&tx, ItemQuery::LocationFlags(ship, &[FLAG_CARGO])).await.unwrap();
    assert_eq!(items.iter().map(|item| i128::from(item.quantity)).sum::<i128>(), 4);
    let full = [super::super::hold_storage::Addition { type_id: cargo_type, singleton: false, quantity: 4 }];
    let ids: Vec<_> = items.iter().map(|item| item.item_id).collect();
    assert!(super::super::hold_storage::check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &full, Replaced { items: &ids, reloads: &[] }, Some(0.000001)).await.is_ok());
    let nested = crate::dao::items::transfers::insert(&tx, ship_type, 1, ship, FLAG_CARGO, 1).await.unwrap();
    let hull = load_items(&tx, &sde, ItemQuery::Id(ship)).await.unwrap().pop().unwrap();
    let mut refused: Outbox = Outbox::default();
    assert!(move_test(&tx, &sde, &mut refused, &hull, &Destination::owned(1, nested, FLAG_CARGO), None, Custody::Preserve).await.is_err());
    assert!(refused.into_entries().is_empty());
    let unchanged = load_items(&tx, &sde, ItemQuery::Id(ship)).await.unwrap().pop().unwrap();
    assert_eq!((unchanged.location_id, unchanged.flag_id), (1, FLAG_HANGAR));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn exact_nonincreasing_hold_replacements() {
    use super::super::hold_storage::{check_destination, Addition};
    use infinity_core::consts::FLAG_DRONE_BAY;
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let fractional = sde.types.iter().find(|t| t.volume == Some(0.01) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE)).unwrap().type_id;
    let other = sde.types.iter().find(|t| t.volume == Some(1.0) && t.category_id == Some(infinity_core::consts::CATEGORY_CHARGE) && t.type_id != fractional).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::transfers::insert(&tx, ship_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&ship]).await.unwrap();
    let stack = crate::dao::items::transfers::insert(&tx, fractional, 1, ship, FLAG_CARGO, 100000).await.unwrap();
    let split = [
        Addition { type_id: fractional, singleton: false, quantity: 99992 },
        Addition { type_id: fractional, singleton: false, quantity: 8 },
    ];
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &split, Replaced { items: &[stack], reloads: &[] }, Some(1.0)).await.is_ok());
    let growth = [Addition { type_id: fractional, singleton: false, quantity: 100001 }];
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &growth, Replaced { items: &[stack], reloads: &[] }, Some(1.0)).await.is_err());
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &split, Replaced { items: &[stack], reloads: &[] }, Some(0.0)).await.is_err());
    for quantity in [0, -1] {
        let invalid = [
            Addition { type_id: fractional, singleton: false, quantity: 1 },
            Addition { type_id: fractional, singleton: false, quantity },
        ];
        assert!(check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &invalid, Replaced { items: &[stack], reloads: &[] }, Some(1.0)).await.is_err());
    }
    let overflow = [
        Addition { type_id: fractional, singleton: false, quantity: i64::MAX },
        Addition { type_id: fractional, singleton: false, quantity: 1 },
    ];
    assert!(matches!(check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &overflow, Replaced { items: &[stack], reloads: &[] }, Some(1.0)).await,
        Err(crate::services::CallError::User { msg, .. }) if msg == infinity_core::messages::MSG_NOT_ENOUGH_CARGO_SPACE));
    tx.execute("DELETE FROM game.items WHERE item_id = $1", &[&stack]).await.unwrap();
    let small = crate::dao::items::transfers::insert(&tx, fractional, 1, ship, FLAG_CARGO, 1).await.unwrap();
    crate::dao::items::transfers::insert(&tx, other, 1, ship, FLAG_CARGO, 1000).await.unwrap();
    let replacement = [Addition { type_id: fractional, singleton: false, quantity: 1 }];
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_CARGO, &replacement, Replaced { items: &[small], reloads: &[] }, Some(1.0)).await.is_ok());
    let drone_stack = crate::dao::items::transfers::insert(&tx, fractional, 1, ship, FLAG_DRONE_BAY, 1).await.unwrap();
    assert!(!super::super::holds::accepts(&sde, FLAG_DRONE_BAY, fractional));
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_DRONE_BAY, &replacement, Replaced { items: &[drone_stack], reloads: &[] }, Some(1.0)).await.is_err());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn exact_singleton_hold_replacements() {
    use super::super::hold_storage::{check_destination, Addition};
    use infinity_core::consts::FLAG_SHIP_HANGAR;
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = crate::db::Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.volume.is_some_and(|v| v > 0.0)
        && super::super::holds::accepts(&sde, FLAG_SHIP_HANGAR, t.type_id)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let ship = crate::dao::items::transfers::insert(&tx, ship_type, 1, 1, FLAG_HANGAR, 1).await.unwrap();
    let first = crate::dao::items::transfers::insert(&tx, ship_type, 1, ship, FLAG_SHIP_HANGAR, 1).await.unwrap();
    let second = crate::dao::items::transfers::insert(&tx, ship_type, 1, ship, FLAG_SHIP_HANGAR, 1).await.unwrap();
    tx.execute("UPDATE game.items SET singleton = true WHERE item_id = ANY($1)", &[&&[ship, first, second][..]]).await.unwrap();
    let addition = || Addition { type_id: ship_type, singleton: true, quantity: 1 };
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_SHIP_HANGAR, &[addition(), addition()], Replaced { items: &[first, second], reloads: &[] }, Some(1.0)).await.is_ok());
    assert!(check_destination(&tx, &sde, 1, ship, FLAG_SHIP_HANGAR, &[addition(), addition(), addition()], Replaced { items: &[first, second], reloads: &[] }, Some(1.0)).await.is_err());
    tx.rollback().await.unwrap();
}
