//! Real-database regression for GM reload reservation refunds.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE"]
async fn gm_reload_refunds_old_reservation_into_existing_cargo_stack() {
    let mut f = crate::services::drones::tests::fixture::Fixture::new().await;
    let scenario = async {
    let (module_type, charge_type) = compatible_ammunition(&f.sde);
    let old_type = f.sde.types.iter().find(|t| t.type_id != charge_type && t.published == Some(true)
        && t.category_id == Some(CATEGORY_CHARGE) && f.sde.is_tradable(t.type_id))
        .expect("second charge type").type_id;
    let ship = f.ship;
    let module = crate::dao::items::creation::rookie_ship(&f.db, module_type, f.owner, ship, infinity_core::consts::FLAG_FITTING_FIRST).await.unwrap();
    f.set(ship, ATTR_CAPACITY, 1_000_000.0).await;
    f.set(module, ATTR_RELOAD_TIME, 60_000.0).await;
    let stack = crate::dao::items::transfers::insert(&f.db, old_type, f.owner, ship, FLAG_CARGO, 10).await.unwrap();
    let old = crate::dao::items::pending_reloads::insert(&f.db, f.owner, ship, module, infinity_core::consts::FLAG_FITTING_FIRST, old_type, 7, FLAG_CARGO, ship).await.unwrap();
    let snapshot_sql = "SELECT json_build_array(
        (SELECT COALESCE(json_agg(row_to_json(i) ORDER BY i.item_id), '[]'::json) FROM game.items i WHERE i.owner_id=$1 OR i.location_id=$2 OR i.item_id=$2),
        (SELECT COALESCE(json_agg(row_to_json(a) ORDER BY a.item_id, a.attribute_id), '[]'::json) FROM game.item_attributes a WHERE a.item_id IN (SELECT i.item_id FROM game.items i WHERE i.owner_id=$1 OR i.location_id=$2 OR i.item_id=$2)),
        (SELECT COALESCE(json_agg(row_to_json(p) ORDER BY p.reload_id), '[]'::json) FROM game.pending_reloads p WHERE p.owner_id=$1 OR p.ship_id=$2),
        (SELECT COALESCE(json_agg(row_to_json(c) ORDER BY c.ship_id, c.flag_id), '[]'::json) FROM game.module_charges c WHERE c.ship_id=$2)
    )::text AS snapshot";
    let before: String = f.db.query_one(snapshot_sql, &[&f.owner, &ship]).await.unwrap().get("snapshot");
    {
        let future = gm_load_ammo(&mut f.client, ship, vec![module], ship, charge_type, 1);
        tokio::pin!(future);
        let guard = crate::services::market::lock().await;
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut future).await.is_err(), "GM reload completed while the economy lock was held");
        let held: String = f.db.query_one(snapshot_sql, &[&f.owner, &ship]).await.unwrap().get("snapshot");
        assert_eq!(held, before, "GM reload changed fixture state while the economy lock was held");
        drop(guard);
        assert_eq!(future.await.unwrap(), 1);
    }
    let rows: Vec<_> = load_items(&f.db, &f.sde, ItemQuery::OwnerLocationFlag(f.owner, ship, FLAG_CARGO)).await.unwrap()
        .into_iter().filter(|item| item.type_id == old_type && !item.singleton).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].item_id, rows[0].quantity), (stack, 17));
    let pending = crate::dao::items::pending_reloads::for_ship(&f.db, ship).await.unwrap();
    assert!(!pending.iter().any(|row| row.reload_id == old));
    };
    let result = crate::tests::catch_scenario(scenario).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
