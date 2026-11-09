//! Real transaction contention between a selected unload and a box deposit.
use super::*;
use crate::dao::items::{creation, pending_reloads, transfers};
use crate::services::inventory::destination::Destination;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn selected_unload_waits_for_last_space_deposit_and_rolls_back_all_modules() {
    let mut f = crate::services::drones::tests::fixture::Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let (module_type, charge_type) = compatible_ammunition(&f.sde);
        let volume = f.sde.types.get(charge_type).unwrap().volume.unwrap();
        let (box_type, quantity) = f.sde.types.iter().filter(|t| t.published == Some(true)
            && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g)))
            .find_map(|t| {
                let capacity = crate::services::inventory::hold_storage::static_capacity(&f.sde, t.type_id).ok()?;
                let n = (capacity / volume).floor();
                let fill = n - 1.0;
                (fill >= 1.0 && fill < f64::from(i32::MAX)
                    && capacity - fill * volume >= volume && capacity - fill * volume < 2.0 * volume)
                    .then_some((t.type_id, fill as i64))
            }).expect("ordinary container with a representable last-space fill");
        let ship = f.ship;
        let box_id = creation::positioned(&f.db, box_type, f.owner, f.system, [0.0; 3]).await.unwrap();
        let mut ball = f.world.ball(f.system, ship).await.unwrap();
        ball.id = box_id;
        ball.type_id = box_type;
        ball.char_id = None;
        ball.radius = f.sde.types.get(box_type).and_then(|t| t.radius).unwrap_or(1.0) as f32;
        f.world.add_ship(f.system, ball).await.unwrap();
        let flag = infinity_core::consts::FLAG_FITTING_FIRST;
        let first = creation::rookie_ship(&f.db, module_type, f.owner, ship, flag).await.unwrap();
        let second = creation::rookie_ship(&f.db, module_type, f.owner, ship, flag + 1).await.unwrap();
        let source = transfers::insert(&f.db, charge_type, f.owner, ship, FLAG_CARGO, 5).await.unwrap();
        crate::dao::items::combat::apply_loads(&f.db, ship, &[(first, flag, charge_type, 1, 0), (second, flag + 1, charge_type, 1, 0)]).await.unwrap();
        pending_reloads::insert(&f.db, f.owner, ship, first, flag, charge_type, 1, FLAG_CARGO, ship).await.unwrap();
        let pending = pending_reloads::for_ship(&f.db, ship).await.unwrap();
        let ids = [ship, box_id, first, second, source];
        let before: Vec<_> = load_items(&f.db, &f.sde, ItemQuery::Ids(&ids)).await.unwrap().iter().map(Item::row).collect();
        let outbox_before = f.client.outbox.len();
        let config = crate::config::tests::test_database().to_pg_config().unwrap();
        let (mut holder_client, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let holder = holder_client.transaction().await.unwrap();
        let mut deposit_outbox: Outbox = Outbox::default();
        let deposited = crate::services::items::deposit(&holder, &f.sde, &mut deposit_outbox,
            &Destination::owned(f.owner, box_id, FLAG_CARGO), charge_type, quantity, 0).await.unwrap();
        let pid = crate::dao::items::parent_locks::tests::backend_pid(&holder).await;
        let (probe, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(async move { connection.await.unwrap(); });
        let conflict = probe.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR SHARE NOWAIT", &[&box_id]).await;
        assert!(matches!(conflict, Err(error) if error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)));
        let db = f.db.clone();
        let args = [Value::Int(ship), Value::list([Value::Int(first), Value::Int(second)]),
            Value::tuple([Value::Int(box_id), Value::Int(f.owner), Value::Int(i64::from(FLAG_CARGO))])];
        let kwargs = Value::None;
        let call = Call { service: "dogmaIM", method: "UnloadAmmo", args: &args, kwargs: &kwargs, bound: true, bind_params: None };
        let operation = crate::services::dispatch(&mut f.client, &call);
        let release = async {
            crate::dao::items::parent_locks::tests::wait_blocked(&db, pid).await;
            holder.commit().await.unwrap();
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(operation, release) }).await.unwrap();
        assert!(outcome.is_err());
        assert_eq!(f.client.outbox.len(), outbox_before);
        assert!(pending_reloads::for_ship(&f.db, ship).await.unwrap() == pending);
        let after: Vec<_> = load_items(&f.db, &f.sde, ItemQuery::Ids(&ids)).await.unwrap().iter().map(Item::row).collect();
        assert_eq!(loaded(&f.db, ship, flag).await.unwrap(), Some((charge_type, 1)));
        assert_eq!(loaded(&f.db, ship, flag + 1).await.unwrap(), Some((charge_type, 1)));
        assert_eq!(after, before);
        let contents = load_items(&f.db, &f.sde, ItemQuery::Location(box_id)).await.unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!((contents[0].item_id, contents[0].type_id, contents[0].quantity), (deposited.last().item_id, charge_type, quantity));
        assert_eq!(deposit_outbox.into_entries().len(), 1);
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn internal_generated_box_source_refusal_rolls_back() {
    let mut f = crate::services::drones::tests::fixture::Fixture::new().await;
    let result = crate::tests::catch_scenario(async {
        let (module_type, charge_type) = compatible_ammunition(&f.sde);
        let volume = f.sde.types.get(charge_type).unwrap().volume.unwrap();
        let box_type = f.sde.types.iter().find(|t| t.published == Some(true)
            && t.group_id.is_some_and(|g| infinity_core::consts::CARGO_CONTAINER_GROUPS.contains(&g))
            && crate::services::inventory::hold_storage::static_capacity(&f.sde, t.type_id)
                .is_ok_and(|capacity| capacity >= volume)).expect("ordinary box with charge space").type_id;
        let ship = f.ship;
        let flag = infinity_core::consts::FLAG_FITTING_FIRST;
        let module = creation::rookie_ship(&f.db, module_type, f.owner, ship, flag).await.unwrap();
        let box_id = creation::rookie_ship(&f.db, box_type, f.owner, ship, FLAG_CARGO).await.unwrap();
        f.set(ship, ATTR_CAPACITY, 1_000_000.0).await;
        transfers::insert(&f.db, charge_type, f.owner, ship, FLAG_CARGO, 5).await.unwrap();
        pending_reloads::insert(&f.db, f.owner, ship, module, flag, charge_type, 1, FLAG_CARGO, ship).await.unwrap();
        let pending = pending_reloads::for_ship(&f.db, ship).await.unwrap();
        let before: Vec<_> = load_items(&f.db, &f.sde, ItemQuery::Owner(f.owner)).await.unwrap().iter().map(Item::row).collect();
        let magazine = loaded(&f.db, ship, flag).await.unwrap();
        let outbox_before = f.client.outbox.len();
        let outcome = gm_load_ammo(&mut f.client, ship, vec![module], box_id, charge_type, 1).await;
        assert!(outcome.is_err());
        assert_eq!(f.client.outbox.len(), outbox_before);
        assert_eq!(loaded(&f.db, ship, flag).await.unwrap(), magazine);
        assert!(pending_reloads::for_ship(&f.db, ship).await.unwrap() == pending);
        let after: Vec<_> = load_items(&f.db, &f.sde, ItemQuery::Owner(f.owner)).await.unwrap().iter().map(Item::row).collect();
        assert_eq!(after, before);
        assert!(load_items(&f.db, &f.sde, ItemQuery::Location(box_id)).await.unwrap().is_empty());
    }).await;
    f.cleanup().await;
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
