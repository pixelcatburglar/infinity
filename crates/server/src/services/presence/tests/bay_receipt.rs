//! Cancellation of bound maintenance-bay boarding receipts before session finish.
//! Exercises StoreVessel and BoardStoredShip with SDE ships and typed item DAOs.

use super::boarding_receipt::{cancel_after_receipt, cargo, cargo_retained, free_ship, leave_and_select, prepare};
use infinity_core::consts::{CATEGORY_SHIP, FLAG_SHIP_HANGAR, GROUP_CAPSULE};
use infinity_core::consts::dogma::ATTR_SHIP_MAINTENANCE_BAY_CAPACITY;
use crate::dao::items::{creation, loading::{load_items, ItemQuery}};
use crate::services::drones::tests::fixture::Fixture;
use marshal::Value;

async fn exercise(store: bool) {
    let mut f = Fixture::real_system().await;
    let host_type = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).is_some_and(|v| v.is_finite() && v > 0.0)).unwrap();
    let capacity = host_type.attribute(ATTR_SHIP_MAINTENANCE_BAY_CAPACITY).unwrap();
    let host_kind = host_type.type_id;
    let kind = f.sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
        && t.group_id != Some(GROUP_CAPSULE) && t.volume.is_some_and(|v| v > 0.0 && v <= capacity)
        && crate::services::inventory::holds::accepts(&f.sde, FLAG_SHIP_HANGAR, t.type_id)
        && f.sde.is_tradable(t.type_id)).unwrap().type_id;
    prepare(&mut f, kind).await;
    let host = free_ship(&f, host_kind).await;
    let target = if store { free_ship(&f, kind).await } else {
        creation::rookie_ship(&f.db, kind, f.owner, host, FLAG_SHIP_HANGAR).await.unwrap()
    };
    let old = f.ship;
    let cargo_id = cargo(&f).await;
    let wallet = crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap();
    let method = if store { "StoreVessel" } else { "BoardStoredShip" };
    let args = if store { vec![Value::Int(host)] } else { vec![Value::Int(host), Value::Int(target)] };
    let (selected, armor) = cancel_after_receipt(&mut f, method, &args, true).await;
    let selected_row = load_items(&f.db, ItemQuery::Id(selected)).await.unwrap().pop().unwrap();
    let item = load_items(&f.db, ItemQuery::Id(old)).await.unwrap().pop().unwrap();
    let old_ball = f.world.ball(f.system, old).await;
    let committed = if store {
        selected_row.type_id == infinity_core::consts::TYPE_CAPSULE
            && item.location_id == host && item.flag_id == FLAG_SHIP_HANGAR && old_ball.is_none()
    } else {
        selected == target && item.location_id == f.system && old_ball.is_some_and(|b| b.char_id.is_none())
    };
    let result = leave_and_select(&mut f, selected, armor).await;
    let balance = crate::dao::wallets::characters::balance(&f.db, f.owner).await.unwrap();
    let cargo_ok = cargo_retained(&f, cargo_id, old).await;
    let remaining = load_items(&f.db, ItemQuery::Ids(&[old, host, target])).await.unwrap().len();
    f.cleanup().await;
    assert!(committed, "incorrect old hull state after boarding");
    assert_eq!(balance, wallet);
    assert!(cargo_ok);
    assert_eq!(remaining, 3);
    assert_eq!(result, Ok(()));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn cancelled_store_receipt_is_checkpointed_before_reselection() { exercise(true).await; }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with real SDE; run serially"]
async fn cancelled_stored_board_receipt_is_checkpointed_before_reselection() { exercise(false).await; }
