use super::*;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_SHIP, FLAG_CARGO, FLAG_SPECIALIZED_AMMO_HOLD};
use infinity_core::consts::dogma::ATTR_SPECIAL_AMMO_HOLD_CAPACITY;

mod container_capacity;

async fn lock_take<'tx>(
    tx: &'tx (impl Executor + ?Sized), sde: &Sde, take: &TakePreparation,
) -> Result<LockedItems<'tx>, CallError> {
    use crate::services::inventory::destination::{lock_items, parent_requests};
    let ancestors = crate::dao::items::parent_locks::capture(tx, take.ancestors()).await.map_err(db_err)?;
    let mut requests = parent_requests(sde, ancestors.iter().map(|row| (row.item_id, row.type_id)), LockStrength::Update, LockStrength::Share)?;
    requests.extend(take.closure().iter().map(|&item_id| LockRequest { item_id, strength: LockStrength::Update }));
    let mut mandatory: Vec<_> = requests.iter().map(|request| request.item_id).collect();
    mandatory.extend(take.refund().mandatory_ids());
    requests.extend_from_slice(take.refund().requests());
    let optional: Vec<_> = take.refund().candidate_ids().into_iter().filter(|id| !mandatory.contains(id)).collect();
    lock_items(tx, sde, &requests, &optional).await
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn recovery_preserves_source_and_retains_refused_rows() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE) && t.volume.is_some_and(|v| v > 0.0)).unwrap();
    let later_charge = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_CHARGE) && t.type_id != charge.type_id && t.volume.is_some_and(|v| v > 0.0)).unwrap();
    let ship_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SPECIAL_AMMO_HOLD_CAPACITY).is_some_and(|v| v >= charge.volume.unwrap())
        && t.capacity.is_some_and(|capacity| capacity.is_finite() && capacity >= charge.volume.unwrap()
            && capacity / charge.volume.unwrap() < i64::MAX as f64 / 2.0)).unwrap().type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('reload-recovery-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let owner: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'reload-recovery-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ship: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, 1, 0, 1, true) RETURNING item_id", &[&ship_type, &owner]).await.unwrap().get("item_id");
    let other: i64 = db.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, 1, 0, 1, true) RETURNING item_id", &[&ship_type, &owner]).await.unwrap().get("item_id");
    let station = sde.map.stations().next().unwrap().station_id;
    let result = crate::tests::catch_scenario(async {
    let ammo = pending_reloads::insert(&db, owner, ship, 1, 1, charge.type_id, 1, FLAG_SPECIALIZED_AMMO_HOLD, ship).await.unwrap();
    let legacy: i64 = db.query_one("INSERT INTO game.pending_reloads (owner_id, ship_id, module_id, flag_id, type_id, quantity, source_location_id) VALUES ($1, $2, 1, 1, $3, 1, $2) RETURNING reload_id", &[&owner, &ship, &charge.type_id]).await.unwrap().get("reload_id");
    let hangar = pending_reloads::insert(&db, owner, other, 1, 1, charge.type_id, 1, FLAG_HANGAR, station).await.unwrap();
    let vanished = pending_reloads::insert(&db, owner, other, 1, 1, charge.type_id, 1, FLAG_CARGO, i64::MAX).await.unwrap();
    let rows = pending_reloads::for_ship(&db, ship).await.unwrap();
    assert_eq!(rows.iter().find(|r| r.reload_id == legacy).unwrap().source_flag, FLAG_CARGO);
    assert_eq!(rows.iter().find(|r| r.reload_id == legacy).unwrap().source_location_id, ship);
    let excess = (sde.types.get(ship_type).unwrap().capacity.unwrap() / charge.volume.unwrap()).ceil() as i64 + 2;
    let refused = pending_reloads::insert(&db, owner, ship, 1, 1, charge.type_id, excess, FLAG_CARGO, i64::MAX).await.unwrap();
    let later = pending_reloads::insert(&db, owner, other, 1, 1, later_charge.type_id, 2, FLAG_HANGAR, station).await.unwrap();
    assert!(later > refused);
    recover_pending_reloads(&db, &sde).await.unwrap();
    let rows = pending_reloads::for_ship(&db, ship).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reload_id, refused);
    assert!(!rows.iter().any(|r| r.reload_id == ammo));
    let returned = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::LocationFlags(ship, &[FLAG_SPECIALIZED_AMMO_HOLD])).await.unwrap();
    assert_eq!(returned.iter().map(|item| i128::from(item.quantity)).sum::<i128>(), 1);
    let legacy_cargo = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::LocationFlags(ship, &[FLAG_CARGO])).await.unwrap();
    assert!(legacy_cargo.iter().any(|item| item.type_id == charge.type_id && item.quantity == 1));
    assert!(pending_reloads::for_ship(&db, other).await.unwrap().is_empty());
    let hangar_items = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::LocationOwnerFlags(station, owner, &[FLAG_HANGAR])).await.unwrap();
    assert!(hangar_items.iter().any(|item| item.type_id == charge.type_id && item.quantity == 1));
    assert_eq!(hangar_items.iter().filter(|item| item.type_id == later_charge.type_id).map(|item| i128::from(item.quantity)).sum::<i128>(), 2);
    let cargo_items = crate::dao::items::loading::load_items(&db, crate::dao::items::loading::ItemQuery::LocationFlags(other, &[FLAG_CARGO])).await.unwrap();
    assert!(cargo_items.iter().any(|item| item.type_id == charge.type_id && item.quantity == 1));
    assert_ne!(hangar, vanished);
    }).await;
    db.execute("DELETE FROM game.pending_reloads WHERE ship_id = ANY($1)", &[&&[ship, other][..]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = ANY($1) OR location_id = ANY($1)", &[&&[ship, other][..]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE owner_id = $1 AND location_id = $2", &[&owner, &station]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database"]
async fn take_preparation_refuses_a_new_intersecting_reservation() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Sde::load(&db).await.unwrap();
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let root = crate::dao::items::creation::rookie_ship(&db, ship_type, 101, 1, 0).await.unwrap();
    let preparation = TakePreparation::capture(&db, &sde, root).await.unwrap();
    let id = pending_reloads::insert(&db, 101, root, 1, 1, 1, 1, FLAG_CARGO, root).await.unwrap();
    assert!(preparation.revalidate(&db).await.is_err());
    pending_reloads::take(&db, id).await.unwrap();
    crate::dao::items::inventory::delete(&db, root).await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn subtree_settlement_intersects_weapon_or_source_and_routes_private_refunds() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|volume| volume > 0.0)).unwrap();
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.capacity.is_some_and(|capacity| capacity.is_finite() && capacity >= charge.volume.unwrap() * 2.0
            && capacity / charge.volume.unwrap() < i64::MAX as f64 / 2.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let mut tx = client.transaction().await.unwrap();
    let owner = 101_i64;
    let actor = 102_i64;
    let root = crate::dao::items::creation::rookie_ship(&tx, ship_type, owner, 1, 0).await.unwrap();
    let outside = crate::dao::items::creation::rookie_ship(&tx, ship_type, owner, 1, 0).await.unwrap();
    let first = pending_reloads::insert(&tx, owner, root, 1, 1, charge.type_id, 1, FLAG_CARGO, outside).await.unwrap();
    let second = pending_reloads::insert(&tx, owner, outside, 2, 2, charge.type_id, 1, FLAG_CARGO, root).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let mut take = TakePreparation::capture(&tx, &sde, root).await.unwrap();
    let locks = lock_take(&tx, &sde, &take).await.unwrap();
    settle_subtree(&tx, &sde, &mut outbox, root, 1, actor, None, &locks, &mut take).await.unwrap();
    drop(locks);
    assert!(pending_reloads::for_ship(&tx, root).await.unwrap().is_empty());
    assert!(pending_reloads::for_ship(&tx, outside).await.unwrap().is_empty());
    assert_ne!(first, second);
    for location in [root, outside] {
        let items = loading::load_items(&tx, ItemQuery::LocationOwnerFlags(location, owner, &[FLAG_CARGO])).await.unwrap();
        assert!(items.iter().any(|item| item.type_id == charge.type_id && item.quantity == 1));
    }
    let (effects, events) = crate::db::outbox::tests::split(outbox);
    assert!(events.is_empty());
    assert_eq!(effects.len(), 2);
    assert!(effects.iter().all(|effect| matches!(effect, crate::db::outbox::Effect::Notification(crate::db::outbox::Notification::Pilot { char_id, .. }) if *char_id == owner)));
    let excess = (sde.types.get(ship_type).unwrap().capacity.unwrap() / charge.volume.unwrap()).ceil() as i64 + 2;
    let retained = pending_reloads::insert(&tx, owner, root, 3, 3, charge.type_id, excess, FLAG_CARGO, i64::MAX).await.unwrap();
    let savepoint = tx.savepoint("overflow").await.unwrap();
    let mut refused: Outbox = Outbox::default();
    let mut take = TakePreparation::capture(&savepoint, &sde, root).await.unwrap();
    let locks = lock_take(&savepoint, &sde, &take).await.unwrap();
    assert!(settle_subtree(&savepoint, &sde, &mut refused, root, 1, actor, None, &locks, &mut take).await.is_err());
    drop(locks);
    let (effects, events) = crate::db::outbox::tests::split(refused);
    assert!(effects.is_empty() && events.is_empty());
    savepoint.rollback().await.unwrap();
    assert!(pending_reloads::for_ship(&tx, root).await.unwrap().iter().any(|row| row.reload_id == retained));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn refund_preparation_shares_receipts_across_ships_and_root_iterations() {
    crate::tests::init_config();
    let config = crate::config::tests::test_database();
    let db = Db::connect(&config).await.unwrap();
    let sde = Arc::new(Sde::load(&db).await.unwrap());
    let charge = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_CHARGE)
        && ty.volume.is_some_and(|volume| volume > 0.0)).unwrap();
    let ship_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_SHIP)
        && ty.capacity.is_some_and(|capacity| capacity >= charge.volume.unwrap() * 3.0)).unwrap().type_id;
    let (mut client, connection) = tokio_postgres::connect(&config.url, tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let owner = 101_i64;
    let destination = crate::dao::items::creation::rookie_ship(&tx, ship_type, owner, 1, 0).await.unwrap();
    let first = crate::dao::items::creation::rookie_ship(&tx, ship_type, owner, 1, 0).await.unwrap();
    let second = crate::dao::items::creation::rookie_ship(&tx, ship_type, owner, 1, 0).await.unwrap();
    let third = crate::dao::items::creation::rookie_ship(&tx, ship_type, owner, 1, 0).await.unwrap();
    let a = pending_reloads::insert(&tx, owner, first, 1, 1, charge.type_id, 1, FLAG_CARGO, destination).await.unwrap();
    let b = pending_reloads::insert(&tx, owner, second, 2, 2, charge.type_id, 1, FLAG_CARGO, destination).await.unwrap();
    let c = pending_reloads::insert(&tx, owner, third, 3, 3, charge.type_id, 1, FLAG_CARGO, destination).await.unwrap();
    let pending = pending_reloads::all(&tx).await.unwrap().into_iter().filter(|row| [a, b, c].contains(&row.reload_id)).collect::<Vec<_>>();
    let mut refund = RefundPreparation::capture_pending(&tx, &sde, &pending).await.unwrap();
    let mut requests: Vec<_> = [destination, first, second, third].into_iter()
        .map(|item_id| LockRequest { item_id, strength: LockStrength::Update }).collect();
    requests.extend_from_slice(refund.requests());
    let locks = crate::services::inventory::destination::lock_items(&tx, &sde, &requests, &[]).await.unwrap();
    let mut outbox: Outbox = Outbox::default();
    let receipts = return_pending(&tx, &sde, &mut outbox, &[a, b], true, &locks, &mut refund).await.unwrap();
    assert_eq!(receipts.len(), 2);
    assert!(receipts[0].created.is_some());
    assert_eq!(receipts[1].merged.as_ref().unwrap().old_quantity, Some(1));
    assert_eq!(receipts[0].last().item_id, receipts[1].last().item_id);
    let more = return_pending(&tx, &sde, &mut outbox, &[c], true, &locks, &mut refund).await.unwrap();
    assert_eq!((more[0].last().item_id, more[0].last().quantity, more[0].merged.as_ref().unwrap().old_quantity), (receipts[0].last().item_id, 3, Some(2)));
    drop(locks);
    tx.rollback().await.unwrap();
}
